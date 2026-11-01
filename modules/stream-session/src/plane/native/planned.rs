//! Synchronous plan capture before any transport mutation.
use super::*;

struct OwnedPlan {
    bytes: Vec<u8>,
    record_starts: Vec<u8>,
    critical_prefix_shards: u32,
    block_count: u32,
    blocks: [VideoFecBlock; VIDEO_FRAME_MAX_BLOCKS],
}
impl OwnedPlan {
    fn header(&self) -> VideoFramePlan {
        VideoFramePlan {
            size: size_of::<VideoFramePlan>() as u32,
            version: 1,
            framed_bytes: BorrowedBytes::new(&self.bytes),
            record_starts: BorrowedBytes::new(&self.record_starts),
            critical_prefix_shards: self.critical_prefix_shards,
            block_count: self.block_count,
            blocks: self.blocks,
        }
    }
}
struct Delivery<'a> {
    limits: &'a VideoFramingLimits,
    calls: usize,
    plan: Option<OwnedPlan>,
    status: Option<Status>,
}
unsafe extern "C" fn capture(context: *mut c_void, plan: *const VideoFramePlan) -> Status {
    if context.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    // SAFETY: callback context lives until the plan call returns.
    let delivery = unsafe { &mut *context.cast::<Delivery<'_>>() };
    delivery.calls += 1;
    let status = if delivery.calls != 1
        || plan.is_null()
        || !(plan as usize).is_multiple_of(align_of::<VideoFramePlan>())
    {
        STATUS_INVALID_ARGUMENT
    } else {
        // SAFETY: trusted provider lends the plan and nested bytes synchronously.
        let plan = unsafe { &*plan };
        if unsafe { plan.validate(delivery.limits) }.is_err() {
            STATUS_INVALID_ARGUMENT
        } else {
            // SAFETY: validation bounded both allocations; provider retains them for the callback.
            let bytes =
                unsafe { std::slice::from_raw_parts(plan.framed_bytes.ptr, plan.framed_bytes.len) };
            let records = if plan.record_starts.len == 0 {
                &[][..]
            } else {
                unsafe {
                    std::slice::from_raw_parts(plan.record_starts.ptr, plan.record_starts.len)
                }
            };
            delivery.plan = Some(OwnedPlan {
                bytes: bytes.to_vec(),
                record_starts: records.to_vec(),
                critical_prefix_shards: plan.critical_prefix_shards,
                block_count: plan.block_count,
                blocks: plan.blocks,
            });
            STATUS_OK
        }
    };
    delivery.status = Some(status);
    status
}

pub(super) fn submit(
    framer: &NativeBinding,
    transport: &NativeBinding,
    limits: &VideoFramingLimits,
    transport_stream: u64,
    unit: &NegotiatedVideoUnit,
) -> PlaneResult<bool> {
    let transport_api = unsafe { transport.table::<NegotiatedVideoPlanTransportApi>() }
        .map_err(|_| "native_layout_mismatch")?;
    if unit.flags == VIDEO_UNIT_END_OF_STREAM {
        let plan = OwnedPlan {
            bytes: Vec::new(),
            record_starts: Vec::new(),
            critical_prefix_shards: 0,
            block_count: 0,
            blocks: [VideoFecBlock::default(); VIDEO_FRAME_MAX_BLOCKS],
        };
        // SAFETY: EOS transport path does not read the empty plan payload.
        return accepted(
            unsafe {
                (transport_api.submit)(transport.context(), transport_stream, unit, &plan.header())
            },
            "native_video_plan_submit_failed",
        );
    }
    let api =
        unsafe { framer.table::<VideoFramingApiV2_1>() }.map_err(|_| "native_layout_mismatch")?;
    let mut delivery = Delivery {
        limits,
        calls: 0,
        plan: None,
        status: None,
    };
    // SAFETY: input unit, limits and stack callback context live throughout the call.
    let status = unsafe {
        (api.base.plan)(
            framer.context(),
            unit.stream,
            unit,
            limits,
            VideoFramePlanSink {
                context: (&mut delivery as *mut Delivery<'_>).cast(),
                write: capture,
            },
        )
    };
    if matches!(status, MEDIA_BUSY | MEDIA_PENDING) && delivery.calls == 0 {
        return Ok(false);
    }
    if status != STATUS_OK || delivery.calls != 1 || delivery.status != Some(STATUS_OK) {
        return Err("video_framer_failed");
    }
    let plan = delivery.plan.ok_or("video_framer_failed")?;
    // SAFETY: plan and unit bytes remain owned until synchronous submit returns.
    accepted(
        unsafe {
            (transport_api.submit)(transport.context(), transport_stream, unit, &plan.header())
        },
        "native_video_plan_submit_failed",
    )
}
