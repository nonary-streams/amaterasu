use super::*;
use crate::abi::{AbiHeader, BorrowedBytes, STATUS_OK, Status, TraceSinkApi};
use std::ffi::c_void;
use std::sync::Mutex;

/// A second sink implementation: it shares no code with the runtime's, so a
/// consumer that works against both depends only on the published contract.
#[derive(Default)]
struct RecordingSink {
    names: Mutex<Vec<String>>,
    records: Mutex<Vec<TraceRecord>>,
    clock: Mutex<u64>,
}

unsafe extern "C" fn now(context: *mut c_void) -> u64 {
    // SAFETY: the context is the test's live sink.
    let sink = unsafe { &*context.cast::<RecordingSink>() };
    let mut clock = sink.clock.lock().unwrap();
    *clock += 1_000;
    *clock
}

unsafe extern "C" fn intern(context: *mut c_void, name: BorrowedBytes, output: *mut u16) -> Status {
    // SAFETY: the context is the test's live sink and the name is borrowed.
    let sink = unsafe { &*context.cast::<RecordingSink>() };
    let name = std::str::from_utf8(unsafe { name.read() }.unwrap()).unwrap();
    let mut names = sink.names.lock().unwrap();
    let index = names
        .iter()
        .position(|known| known == name)
        .unwrap_or_else(|| {
            names.push(name.to_owned());
            names.len() - 1
        });
    // SAFETY: the caller supplied writable output.
    unsafe { *output = index as u16 + 1 };
    STATUS_OK
}

unsafe extern "C" fn record(
    context: *mut c_void,
    records: *const TraceRecord,
    count: usize,
) -> Status {
    // SAFETY: the context is the test's live sink; records are borrowed.
    let sink = unsafe { &*context.cast::<RecordingSink>() };
    let records = unsafe { std::slice::from_raw_parts(records, count) };
    sink.records.lock().unwrap().extend_from_slice(records);
    STATUS_OK
}

fn tracer_for(sink: &RecordingSink, api: &mut Option<Box<TraceSinkApi>>) -> Tracer {
    let table = api.insert(Box::new(TraceSinkApi {
        header: AbiHeader::of::<TraceSinkApi>(),
        context: std::ptr::from_ref(sink).cast_mut().cast(),
        now,
        intern,
        record,
    }));
    // SAFETY: the boxed table and the sink outlive the tracer in each test.
    let bound = unsafe { TraceSink::from_address(std::ptr::from_ref(&**table) as usize) };
    Tracer::with_sink(bound.unwrap())
}

/// The consumer under test. It is written once and never told which sink
/// implementation it is talking to.
fn trace_one_input_event(tracer: &Tracer) {
    let track = tracer.track(names::INPUT_TRACK);
    let dequeued = tracer.stage(names::SESSION_DEQUEUED);
    let accepted = tracer.stage(names::INPUT_ACCEPTED);
    let flow = flow_id(0xABCD, 7);
    let arrival = tracer.now().earlier_by(Duration::from_micros(400));
    tracer.mark_at(track, flow, tracer.stage(names::SOCKET_ARRIVED), arrival);
    tracer.mark_with(track, flow, dequeued, 3);
    tracer.finish(track, flow, accepted, Duration::from_millis(8));
    {
        let _work = tracer.span(tracer.span_kind("input.batch"), tracer.label("keyboard"));
    }
}

#[test]
fn an_unchanged_consumer_records_through_a_replacement_sink() {
    let sink = RecordingSink::default();
    let mut api = None;
    trace_one_input_event(&tracer_for(&sink, &mut api));

    let names = sink.names.lock().unwrap().clone();
    let name = |identifier: u16| names[usize::from(identifier) - 1].as_str();
    let records = sink.records.lock().unwrap().clone();
    assert_eq!(records.len(), 4);
    let flow = flow_id(0xABCD, 7);

    assert_eq!(records[0].kind, TRACE_MARK);
    assert_eq!(name(records[0].track), "input");
    assert_eq!(name(records[0].stage), "socket.arrived");
    // 1_000 from the sink's clock, less the 400 us the datagram waited.
    assert_eq!((records[0].flow, records[0].timestamp_us), (flow, 600));

    assert_eq!(name(records[1].stage), "session.dequeued");
    assert_eq!((records[1].value, records[1].timestamp_us), (3, 0));

    assert_eq!(records[2].kind, TRACE_END);
    assert_eq!(name(records[2].stage), "input.accepted");
    assert_eq!(records[2].value, 8_000);

    assert_eq!(records[3].kind, TRACE_SPAN);
    assert_eq!(name(records[3].track), "input.batch");
    assert_eq!(name(records[3].stage), "keyboard");
    assert_eq!(records[3].flow, 0);
}

#[test]
fn a_disabled_tracer_accepts_the_same_calls_and_records_nothing() {
    let tracer = Tracer::disabled();
    assert!(!tracer.is_enabled());
    assert!(!tracer.track(names::INPUT_TRACK).is_named());
    // The identical consumer code runs without a sink.
    trace_one_input_event(&tracer);
}

#[test]
fn unnamed_tracks_are_skipped_instead_of_being_misattributed() {
    let sink = RecordingSink::default();
    let mut api = None;
    let tracer = tracer_for(&sink, &mut api);
    tracer.mark(Track::default(), 1, tracer.stage("stage"));
    tracer.record_span(
        SpanKind::default(),
        Label::default(),
        Duration::from_millis(1),
    );
    drop(tracer.span(SpanKind::default(), Label::default()));
    assert!(sink.records.lock().unwrap().is_empty());
}

#[test]
fn flow_identities_agree_across_modules_and_differ_within_a_stream() {
    assert_eq!(flow_id(11, 5), flow_id(11, 5));
    assert_ne!(flow_id(11, 5), flow_id(11, 6));
    assert_ne!(flow_id(11, 5), flow_id(12, 5));
    assert_ne!(flow_id(0, 0), flow_id(0, 1));
}

#[test]
fn an_incompatible_or_missing_table_leaves_tracing_disabled() {
    // SAFETY: zero and misaligned addresses are rejected before any read.
    assert!(unsafe { TraceSink::from_address(0) }.is_none());
    assert!(unsafe { TraceSink::from_address(1) }.is_none());
    #[repr(C, align(8))]
    struct Short(AbiHeader);
    let short = Short(AbiHeader {
        size: 8,
        ..AbiHeader::of::<TraceSinkApi>()
    });
    // SAFETY: a readable header is all the validation reads for a short table.
    assert!(unsafe { TraceSink::from_address(std::ptr::from_ref(&short) as usize) }.is_none());
}
