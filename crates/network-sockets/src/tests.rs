use super::*;
struct Bound {
    pointer: *const net::NetworkApi,
}
impl Bound {
    fn new(factory: &Factory) -> Self {
        // SAFETY: tests retain factory until bindings end.
        Self {
            pointer: unsafe { bind_standalone(factory, "test.client").unwrap() },
        }
    }
    fn api(&self) -> &net::NetworkApi {
        // SAFETY: live unique binding until Drop.
        unsafe { &*self.pointer }
    }
    fn context(&self) -> *mut c_void {
        self.api().base.context
    }
    fn udp(&self) -> u64 {
        let mut id = 0; // SAFETY: valid local address, binding and writable output.
        assert_eq!(
            unsafe {
                (self.api().udp_bind)(
                    self.context(),
                    to_address("127.0.0.1:0".parse().unwrap()),
                    &mut id,
                )
            },
            STATUS_OK
        );
        id
    }
    fn local(&self, id: u64) -> SocketAddr {
        let mut address = net::Address::default(); // SAFETY: binding and output live during callback.
        assert_eq!(
            unsafe { (self.api().local_addr)(self.context(), id, &mut address) },
            STATUS_OK
        );
        from_address(address).unwrap()
    }
}
impl Drop for Bound {
    fn drop(&mut self) {
        // SAFETY: no active calls and exactly one release.
        unsafe { (self.api().base.release.unwrap())(self.context()) };
    }
}
#[test]
fn opaque_binding_handles_and_aggregate_capacity_are_enforced() {
    let factory = Factory::with_limits(2);
    let first = Bound::new(&factory);
    let second = Bound::new(&factory);
    let id = first.udp();
    let mut address = net::Address::default();
    assert_eq!(
        unsafe { (second.api().local_addr)(second.context(), id, &mut address) },
        net::NOT_FOUND
    );
    let second_id = second.udp();
    let mut excessive = 0;
    assert_eq!(
        unsafe {
            (first.api().udp_bind)(
                first.context(),
                to_address("127.0.0.1:0".parse().unwrap()),
                &mut excessive,
            )
        },
        net::RESOURCE_EXHAUSTED
    );
    assert_eq!(
        unsafe { (first.api().close)(first.context(), id) },
        STATUS_OK
    );
    let third = first.udp();
    assert_ne!(third, id);
    assert_eq!(
        unsafe { (first.api().close)(first.context(), id) },
        net::NOT_FOUND
    );
    assert_eq!(
        unsafe { (second.api().close)(second.context(), second_id) },
        STATUS_OK
    );
}
#[test]
fn udp_connected_batch_preserves_whole_datagrams_and_rejects_oversized_buffers() {
    let factory = Factory::new();
    let provider = Bound::new(&factory);
    let id = provider.udp();
    let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
    receiver
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert_eq!(
        unsafe {
            (provider.api().connect_udp)(
                provider.context(),
                id,
                to_address(receiver.local_addr().unwrap()),
            )
        },
        STATUS_OK
    );
    let first = b"one";
    let second = b"two";
    let input = [
        net::Datagram {
            data: first.as_ptr(),
            len: first.len(),
        },
        net::Datagram {
            data: second.as_ptr(),
            len: second.len(),
        },
    ];
    let mut count = 0;
    assert_eq!(
        unsafe {
            (provider.api().send_batch)(
                provider.context(),
                id,
                input.as_ptr(),
                input.len(),
                &mut count,
            )
        },
        STATUS_OK
    );
    assert_eq!(count, 2);
    let mut bytes = [0; 64];
    let (len, _) = receiver.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..len], b"one");
    let (len, _) = receiver.recv_from(&mut bytes).unwrap();
    assert_eq!(&bytes[..len], b"two");
    assert_eq!(
        unsafe {
            (provider.api().write)(
                provider.context(),
                id,
                first.as_ptr(),
                net::MAX_IO_BYTES + 1,
                &mut count,
            )
        },
        STATUS_INVALID_ARGUMENT
    );
}
#[test]
fn tcp_listener_connection_peek_and_shutdown_use_the_same_contract() {
    let factory = Factory::new();
    let provider = Bound::new(&factory);
    let mut listener = 0;
    let options = net::ListenOptions {
        size: size_of::<net::ListenOptions>() as u32,
        flags: net::LISTEN_NONBLOCKING | net::LISTEN_IPV6_ONLY,
        backlog: 16,
        reserved: 0,
    };
    assert_eq!(
        unsafe {
            (provider.api().tcp_listen)(
                provider.context(),
                to_address("127.0.0.1:0".parse().unwrap()),
                options,
                &mut listener,
            )
        },
        STATUS_OK
    );
    let mut client = 0;
    assert_eq!(
        unsafe {
            (provider.api().tcp_connect)(
                provider.context(),
                to_address(provider.local(listener)),
                1000,
                &mut client,
            )
        },
        STATUS_OK
    );
    let mut server = 0;
    let mut peer = net::Address::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let status = unsafe {
            (provider.api().accept)(provider.context(), listener, &mut server, &mut peer)
        };
        if status == STATUS_OK {
            break;
        }
        assert_eq!(status, net::WOULD_BLOCK);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now()
    }
    assert!(from_address(peer).unwrap().ip().is_loopback());
    let mut count = 0;
    let bytes = b"hello";
    assert_eq!(
        unsafe {
            (provider.api().write)(
                provider.context(),
                client,
                bytes.as_ptr(),
                bytes.len(),
                &mut count,
            )
        },
        STATUS_OK
    );
    assert_eq!(count, 5);
    let mut output = [0; 5];
    loop {
        let status = unsafe {
            (provider.api().peek)(
                provider.context(),
                server,
                output.as_mut_ptr(),
                output.len(),
                &mut count,
            )
        };
        if status == STATUS_OK {
            break;
        }
        assert_eq!(status, net::WOULD_BLOCK);
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now()
    }
    assert_eq!(&output, b"hello");
    assert_eq!(
        unsafe {
            (provider.api().read)(
                provider.context(),
                server,
                output.as_mut_ptr(),
                output.len(),
                &mut count,
            )
        },
        STATUS_OK
    );
    assert_eq!(&output, b"hello");
    assert_eq!(
        unsafe { (provider.api().shutdown)(provider.context(), client, net::SHUTDOWN_BOTH) },
        STATUS_OK
    );
}

#[test]
fn closing_listener_prevents_inflight_accept_from_publishing_a_child() {
    let factory = Factory::with_limits(2);
    let provider = Bound::new(&factory);
    let mut listener = 0;
    let options = net::ListenOptions {
        size: size_of::<net::ListenOptions>() as u32,
        flags: 0,
        backlog: 16,
        reserved: 0,
    };
    assert_eq!(
        unsafe {
            (provider.api().tcp_listen)(
                provider.context(),
                to_address("127.0.0.1:0".parse().unwrap()),
                options,
                &mut listener,
            )
        },
        STATUS_OK
    );
    let address = provider.local(listener);
    let context = provider.context() as usize;
    let accept = provider.api().accept;
    let worker = std::thread::spawn(move || {
        let mut child = 0;
        let mut peer = net::Address::default();
        // SAFETY: the test retains the binding until the worker joins.
        let status = unsafe { accept(context as *mut c_void, listener, &mut child, &mut peer) };
        (status, child)
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while factory.shared.handles.load(Ordering::Acquire) != 2
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    let entered = factory.shared.handles.load(Ordering::Acquire) == 2;
    if !entered {
        let client = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
        let _ = worker.join();
        drop(client);
        panic!("accept worker did not reach the blocking call");
    }
    let close_status = unsafe { (provider.api().close)(provider.context(), listener) };
    // The in-flight accept retains the listener; connect wakes it even after
    // the handle has been closed. Close itself is not a cancellation API.
    let client = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    let (status, child) = worker.join().unwrap();
    drop(client);
    assert_eq!(close_status, STATUS_OK);
    assert_eq!(status, net::NOT_FOUND);
    assert_eq!(child, 0);
    assert_eq!(factory.shared.handles.load(Ordering::Acquire), 0);
}
#[test]
fn explicitly_nonblocking_connected_tcp_reads_and_peeks_do_not_wait_for_an_idle_peer() {
    for peek in [false, true] {
        let factory = Factory::new();
        let provider = Bound::new(&factory);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = 0;
        assert_eq!(
            unsafe {
                (provider.api().tcp_connect)(
                    provider.context(),
                    to_address(listener.local_addr().unwrap()),
                    1_000,
                    &mut client,
                )
            },
            STATUS_OK
        );
        // Outbound streams start blocking; a polling consumer selects its mode.
        assert_eq!(
            unsafe { (provider.api().set_option)(provider.context(), client, net::NONBLOCKING, 1) },
            STATUS_OK
        );
        listener.set_nonblocking(true).unwrap();
        let accept_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let peer = loop {
            match listener.accept() {
                Ok((peer, _)) => break peer,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < accept_deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("loopback accept failed: {error}"),
            }
        };
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let holding_peer = std::thread::spawn(move || {
            let _ = released.recv_timeout(Duration::from_millis(500));
            drop(peer);
        });
        let mut output = [0; 1];
        let mut count = usize::MAX;
        let started = std::time::Instant::now();
        let operation = if peek {
            provider.api().peek
        } else {
            provider.api().read
        };
        // SAFETY: the live binding and writable buffers outlive this callback.
        let status = unsafe {
            operation(
                provider.context(),
                client,
                output.as_mut_ptr(),
                output.len(),
                &mut count,
            )
        };
        let elapsed = started.elapsed();
        let _ = release.send(());
        holding_peer.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(100),
            "idle TCP operation blocked"
        );
        assert_eq!(status, net::WOULD_BLOCK);
    }
}

#[test]
fn address_value_conversion_preserves_ipv6_scope_and_rejects_noncanonical_ipv4() {
    let scoped = SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 42, 7, 19));
    assert_eq!(from_address(to_address(scoped)).unwrap(), scoped);
    let mut bad = to_address("127.0.0.1:1".parse().unwrap());
    bad.bytes[4] = 1;
    assert!(from_address(bad).is_err());
}
#[test]
fn releasing_binding_closes_owned_sockets_and_returns_budget() {
    let factory = Factory::with_limits(1);
    let bound = Bound::new(&factory);
    let id = bound.udp();
    let address = bound.local(id);
    assert!(UdpSocket::bind(address).is_err());
    drop(bound);
    let socket = UdpSocket::bind(address).unwrap();
    drop(socket);
    let replacement = Bound::new(&factory);
    replacement.udp();
}

#[test]
fn shutdown_announcements_preserve_cleanup_and_quiescence_closes_handle_admission() {
    let factory = Factory::new();
    let binding = Bound::new(&factory);
    factory.prepare_shutdown(1_000).unwrap();
    let existing = binding.udp();
    let address = binding.local(existing);
    assert!(factory.quiesce().unwrap());
    let mut rejected = 0;
    assert_eq!(
        unsafe {
            (binding.api().udp_bind)(
                binding.context(),
                to_address("127.0.0.1:0".parse().unwrap()),
                &mut rejected,
            )
        },
        net::INTERRUPTED
    );
    assert_eq!(rejected, 0);
    assert!(matches!(
        unsafe { bind_standalone(&factory, "late.consumer") },
        Err(net::INTERRUPTED)
    ));
    assert_eq!(
        unsafe { (binding.api().close)(binding.context(), existing) },
        STATUS_OK
    );
    assert!(UdpSocket::bind(address).is_ok());
}

#[test]
fn dns_numeric_address_and_unsupported_options_are_explicit() {
    let factory = Factory::new();
    let provider = Bound::new(&factory);
    let mut addresses = [net::Address::default(); 4];
    let mut count = 0;
    // SAFETY: all output storage and borrowed input survive the callback.
    assert_eq!(
        unsafe {
            (provider.api().resolve)(
                provider.context(),
                BorrowedBytes::new(b"127.0.0.1"),
                42,
                1000,
                addresses.as_mut_ptr(),
                addresses.len(),
                &mut count,
            )
        },
        STATUS_OK
    );
    assert_eq!(count, 1);
    assert_eq!(
        from_address(addresses[0]).unwrap(),
        "127.0.0.1:42".parse().unwrap()
    );
    let id = provider.udp();
    assert_eq!(
        unsafe { (provider.api().set_option)(provider.context(), id, u32::MAX, 0) },
        net::UNSUPPORTED
    );
    assert_eq!(
        unsafe { (provider.api().set_option)(provider.context(), id, net::NONBLOCKING, 2) },
        STATUS_INVALID_ARGUMENT
    );
}

#[test]
fn udp_send_buffer_request_never_shrinks_the_default() {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
    let default = socket.send_buffer_size().unwrap();

    request_udp_send_buffer(&socket);

    assert!(socket.send_buffer_size().unwrap() >= default);
}
