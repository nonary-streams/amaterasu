//! Executable protocol fixture for the ignored administrative integration test.
//! This does not perform driver operations or claim capture hardware coverage.
use std::{ffi::c_void, fs, os::fd::FromRawFd, path::Path, process::Command};

unsafe extern "C" {
    fn read(fd: i32, bytes: *mut c_void, len: usize) -> isize;
    fn write(fd: i32, bytes: *const c_void, len: usize) -> isize;
    fn recvmsg(fd: i32, message: *mut Message, flags: i32) -> isize;
    fn geteuid() -> u32;
    fn getegid() -> u32;
    fn fcntl(fd: i32, operation: i32, ...) -> i32;
}
#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}
#[repr(C)]
struct Message {
    name: *mut c_void,
    name_len: u32,
    vectors: *mut IoVec,
    vector_count: usize,
    control: *mut c_void,
    control_len: usize,
    flags: i32,
}
#[repr(C)]
struct ControlHeader {
    len: usize,
    level: i32,
    kind: i32,
}

fn send(fd: i32, bytes: &[u8]) {
    assert_eq!(
        unsafe { write(fd, bytes.as_ptr().cast(), bytes.len()) },
        bytes.len() as isize
    );
}
fn receive(fd: i32, bytes: &mut [u8]) -> isize {
    unsafe { read(fd, bytes.as_mut_ptr().cast(), bytes.len()) }
}
fn open_session() -> std::fs::File {
    send(198, b"AMACA001");
    let mut bytes = [0u8; 8];
    let mut vector = IoVec {
        base: bytes.as_mut_ptr().cast(),
        len: bytes.len(),
    };
    let mut control = [0usize; 8];
    let mut message = Message {
        name: std::ptr::null_mut(),
        name_len: 0,
        vectors: &mut vector,
        vector_count: 1,
        control: control.as_mut_ptr().cast(),
        control_len: std::mem::size_of_val(&control),
        flags: 0,
    };
    assert_eq!(unsafe { recvmsg(198, &mut message, 0x40000000) }, 8);
    assert_eq!(&bytes, b"AMACAOK1");
    let header = unsafe { &*control.as_ptr().cast::<ControlHeader>() };
    assert_eq!((header.level, header.kind), (1, 1));
    assert_eq!(header.len, std::mem::size_of::<ControlHeader>() + 4);
    let fd = unsafe {
        *control
            .as_ptr()
            .cast::<u8>()
            .add(std::mem::size_of::<ControlHeader>())
            .cast::<i32>()
    };
    assert!(fd >= 3);
    unsafe { std::fs::File::from_raw_fd(fd) }
}
fn request(opcode: u16, id: u32, card: u32) -> [u8; 48] {
    let mut data = [0u8; 48];
    data[..4].copy_from_slice(b"AVFQ");
    data[4..6].copy_from_slice(&1u16.to_le_bytes());
    data[6..8].copy_from_slice(&opcode.to_le_bytes());
    data[8..12].copy_from_slice(&id.to_le_bytes());
    if opcode == 2 {
        data[16..20].copy_from_slice(&226u32.to_le_bytes());
        data[20..24].copy_from_slice(&card.to_le_bytes());
    }
    data
}
fn hello(fd: i32) {
    send(fd, &request(1, 1, 0));
    let mut reply = [0u8; 128];
    assert_eq!(receive(fd, &mut reply), 128);
    assert_eq!(&reply[..4], b"AVFR");
    assert_eq!(u32::from_le_bytes(reply[12..16].try_into().unwrap()), 0);
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--check-fd") {
        std::process::exit(if unsafe { fcntl(198, 1) } < 0 { 0 } else { 9 });
    }
    if args.get(1).map(String::as_str) == Some("--approved-service") {
        helper();
    } else {
        worker(&args);
    }
}
fn worker(args: &[String]) {
    use std::os::fd::AsRawFd;
    assert_eq!(
        args.get(1).map(String::as_str),
        Some("worker-authorized-serve-v2")
    );
    assert_eq!((unsafe { geteuid() }, unsafe { getegid() }), (65534, 65534));
    assert!(std::env::var_os("LD_PRELOAD").is_none());
    assert!(std::env::var_os("AMATERASU_UNREVIEWED_ENVIRONMENT").is_none());
    assert_eq!(unsafe { fcntl(198, 2, 1) }, 0);
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .arg("--check-fd")
            .status()
            .unwrap()
            .success()
    );
    let state = Path::new(&args[4]);
    let package = Path::new(&args[2]).join("sets").join(&args[3]);
    assert!(!package.join("helper-pids").exists());
    precheck();
    assert!(
        !package.join("helper-pids").exists(),
        "precheck must not execute a helper"
    );
    fs::write(state.join("precheck-ready"), b"yes").unwrap();
    let scope = open_session();
    hello(scope.as_raw_fd());
    send(scope.as_raw_fd(), &request(2, 2, 1));
    let mut reply = [0u8; 128];
    assert!(receive(scope.as_raw_fd(), &mut reply) <= 0);
    fs::write(state.join("scope-denied"), b"yes").unwrap();
    let replay = open_session();
    hello(replay.as_raw_fd());
    send(replay.as_raw_fd(), &request(1, 1, 0));
    assert!(receive(replay.as_raw_fd(), &mut reply) <= 0);
    fs::write(state.join("replay-denied"), b"yes").unwrap();
    let active = open_session();
    hello(active.as_raw_fd());
    send(active.as_raw_fd(), &request(2, 2, 0));
    assert_eq!(receive(active.as_raw_fd(), &mut reply), 128);
    assert_eq!(u32::from_le_bytes(reply[12..16].try_into().unwrap()), 0);
    fs::write(state.join("ready"), std::process::id().to_string()).unwrap();
    // The root test withdraws approval. A closed authority session must never
    // produce another valid response; the supervisor also terminates us.
    assert!(receive(active.as_raw_fd(), &mut reply) <= 0);
    fs::write(state.join("session-closed"), b"yes").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(30));
}

fn precheck() {
    send(198, b"AMACAP01");
    let mut bytes = [0u8; 9];
    let mut vector = IoVec {
        base: bytes.as_mut_ptr().cast(),
        len: bytes.len(),
    };
    let mut control = [0usize; 8];
    let mut message = Message {
        name: std::ptr::null_mut(),
        name_len: 0,
        vectors: &mut vector,
        vector_count: 1,
        control: control.as_mut_ptr().cast(),
        control_len: std::mem::size_of_val(&control),
        flags: 0,
    };
    assert_eq!(unsafe { recvmsg(198, &mut message, 0x40000000) }, 8);
    assert_eq!(&bytes[..8], b"AMACAPOK");
    assert_eq!(
        message.control_len, 0,
        "pure precheck carries no descriptors"
    );
}
fn helper() {
    use std::io::Write;
    let executable = std::env::current_exe().unwrap();
    let mut pids = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(executable.parent().unwrap().join("helper-pids"))
        .unwrap();
    writeln!(pids, "{}", std::process::id()).unwrap();
    drop(pids);
    assert_eq!((unsafe { geteuid() }, unsafe { getegid() }), (0, 0));
    loop {
        let mut request = [0u8; 48];
        if receive(3, &mut request) != 48 {
            break;
        }
        let mut response = [0u8; 128];
        response[..4].copy_from_slice(b"AVFR");
        response[4..12].copy_from_slice(&request[4..12]);
        match u16::from_le_bytes(request[6..8].try_into().unwrap()) {
            1 => response[20..24].copy_from_slice(&1u32.to_le_bytes()),
            2 => assert_eq!(u32::from_le_bytes(request[20..24].try_into().unwrap()), 0),
            _ => panic!("fixture helper only supports hello/open"),
        }
        send(3, &response);
    }
}
