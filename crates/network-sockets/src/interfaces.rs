//! Provider-owned host interface discovery. Consumers receive address values,
//! never pointers into OS adapter records or platform-specific interface types.
use amaterasu_network_api::{InterfaceAddress, MAX_ADDRESSES};
use std::io;

pub(crate) fn enumerate() -> io::Result<Vec<InterfaceAddress>> {
    let mut addresses = platform::enumerate()?;
    addresses.sort_by_key(|entry| {
        (
            entry.address.family,
            entry.address.bytes,
            entry.address.scope_id,
            entry.interface_index,
        )
    });
    addresses.dedup_by_key(|entry| {
        (
            entry.address.family,
            entry.address.bytes,
            entry.address.scope_id,
            entry.interface_index,
        )
    });
    if addresses.len() > MAX_ADDRESSES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "network interface address limit exceeded",
        ));
    }
    Ok(addresses)
}

fn push_address(addresses: &mut Vec<InterfaceAddress>, entry: InterfaceAddress) -> io::Result<()> {
    if addresses.iter().any(|previous| {
        previous.interface_index == entry.interface_index && previous.address == entry.address
    }) {
        return Ok(());
    }
    if addresses.len() == MAX_ADDRESSES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "network interface address limit exceeded",
        ));
    }
    addresses.push(entry);
    Ok(())
}

#[cfg(unix)]
mod platform {
    use super::*;
    use amaterasu_network_api::Address;
    use std::ptr;

    struct InterfaceList(*mut libc::ifaddrs);
    impl Drop for InterfaceList {
        fn drop(&mut self) {
            // SAFETY: this is the original allocation returned by successful
            // getifaddrs; ownership is unique and all borrowed nodes have ended.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }

    pub(super) fn enumerate() -> io::Result<Vec<InterfaceAddress>> {
        let mut first = ptr::null_mut();
        // SAFETY: first is writable output storage; no list is traversed unless
        // the OS reports success. The RAII guard owns the allocation thereafter.
        if unsafe { libc::getifaddrs(&mut first) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let list = InterfaceList(first);
        let mut addresses = Vec::new();
        let mut current = list.0;
        while !current.is_null() {
            // SAFETY: current is an initialized node in the live OS list.
            let interface = unsafe { &*current };
            if interface.ifa_flags & libc::IFF_UP as u32 != 0
                && !interface.ifa_addr.is_null()
                && !interface.ifa_name.is_null()
            {
                // SAFETY: ifa_name is a NUL-terminated name owned by list.
                let index = unsafe { libc::if_nametoindex(interface.ifa_name) };
                if index != 0 {
                    // SAFETY: every non-null sockaddr has a readable family.
                    let family = unsafe { (*interface.ifa_addr).sa_family as i32 };
                    let address = match family {
                        libc::AF_INET => {
                            // SAFETY: matching family identifies sockaddr_in.
                            let address =
                                unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in>() };
                            let mut bytes = [0; 16];
                            bytes[..4].copy_from_slice(&address.sin_addr.s_addr.to_ne_bytes());
                            Some(Address {
                                family: 4,
                                bytes,
                                ..Address::default()
                            })
                        }
                        libc::AF_INET6 => {
                            // SAFETY: matching family identifies sockaddr_in6.
                            let address =
                                unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in6>() };
                            Some(Address {
                                family: 6,
                                bytes: address.sin6_addr.s6_addr,
                                scope_id: address.sin6_scope_id,
                                ..Address::default()
                            })
                        }
                        _ => None,
                    };
                    if let Some(address) = address {
                        push_address(
                            &mut addresses,
                            InterfaceAddress {
                                address,
                                interface_index: index,
                                reserved: 0,
                            },
                        )?;
                    }
                }
            }
            current = interface.ifa_next;
        }
        Ok(addresses)
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use amaterasu_network_api::Address;
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        NetworkManagement::{
            IpHelper::{
                GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
                GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
            },
            Ndis::IfOperStatusUp,
        },
        Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC},
    };

    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    const MAX_ADAPTER_BUFFER_BYTES: usize = 1024 * 1024;

    pub(super) fn enumerate() -> io::Result<Vec<InterfaceAddress>> {
        // u64 elements establish the alignment required by adapter records on
        // supported 64-bit Windows. The allocation never crosses the native ABI.
        let mut buffer = vec![0u64; 2048];
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut result = ERROR_BUFFER_OVERFLOW;
        for _ in 0..3 {
            let mut buffer_bytes = (buffer.len() * size_of::<u64>()) as u32;
            // SAFETY: aligned writable buffer advertises its exact capacity;
            // Windows initializes all linked adapter/unicast records on success.
            result = unsafe {
                GetAdaptersAddresses(
                    AF_UNSPEC as u32,
                    flags,
                    ptr::null_mut(),
                    buffer.as_mut_ptr().cast(),
                    &mut buffer_bytes,
                )
            };
            if result != ERROR_BUFFER_OVERFLOW {
                break;
            }
            if buffer_bytes as usize > MAX_ADAPTER_BUFFER_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "network adapter metadata limit exceeded",
                ));
            }
            buffer.resize((buffer_bytes as usize).div_ceil(size_of::<u64>()), 0);
        }
        if result == 232 {
            return Ok(Vec::new());
        }
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let mut addresses = Vec::new();
        let mut adapter = buffer.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !adapter.is_null() {
            // SAFETY: successful enumeration initialized these buffer-owned nodes.
            let adapter_record = unsafe { &*adapter };
            if adapter_record.OperStatus == IfOperStatusUp {
                let mut unicast = adapter_record.FirstUnicastAddress;
                while !unicast.is_null() {
                    // SAFETY: all unicast pointers remain owned by the live buffer.
                    let record = unsafe { &*unicast };
                    let socket = record.Address;
                    if !socket.lpSockaddr.is_null() && socket.iSockaddrLength >= 2 {
                        // SAFETY: Windows supplied a readable sockaddr of this length.
                        let bytes = unsafe {
                            std::slice::from_raw_parts(
                                socket.lpSockaddr.cast::<u8>(),
                                socket.iSockaddrLength as usize,
                            )
                        };
                        let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
                        let item = if family == AF_INET && bytes.len() >= 16 {
                            let mut octets = [0; 16];
                            octets[..4].copy_from_slice(&bytes[4..8]);
                            // SAFETY: adapter's versioned prefix contains IfIndex.
                            let index = unsafe { adapter_record.Anonymous1.Anonymous.IfIndex };
                            Some((
                                Address {
                                    family: 4,
                                    bytes: octets,
                                    ..Address::default()
                                },
                                index,
                            ))
                        } else if family == AF_INET6 && bytes.len() >= 28 {
                            let mut octets = [0; 16];
                            octets.copy_from_slice(&bytes[8..24]);
                            let scope_id = u32::from_ne_bytes(
                                bytes[24..28].try_into().expect("checked sockaddr length"),
                            );
                            Some((
                                Address {
                                    family: 6,
                                    bytes: octets,
                                    scope_id,
                                    ..Address::default()
                                },
                                adapter_record.Ipv6IfIndex,
                            ))
                        } else {
                            None
                        };
                        if let Some((address, interface_index)) = item
                            && interface_index != 0
                        {
                            push_address(
                                &mut addresses,
                                InterfaceAddress {
                                    address,
                                    interface_index,
                                    reserved: 0,
                                },
                            )?;
                        }
                    }
                    unicast = record.Next;
                }
            }
            adapter = adapter_record.Next;
        }
        Ok(addresses)
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(super) fn enumerate() -> io::Result<Vec<InterfaceAddress>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn address_limit_is_checked_before_growing_and_duplicates_do_not_consume_capacity() {
        use amaterasu_network_api::Address;
        let mut entries = Vec::new();
        for index in 0..MAX_ADDRESSES {
            let entry = InterfaceAddress {
                address: Address {
                    family: 6,
                    scope_id: index as u32,
                    ..Address::default()
                },
                interface_index: index as u32 + 1,
                reserved: 0,
            };
            push_address(&mut entries, entry).unwrap();
            push_address(&mut entries, entry).unwrap();
        }
        assert_eq!(entries.len(), MAX_ADDRESSES);
        let excessive = InterfaceAddress {
            interface_index: MAX_ADDRESSES as u32 + 1,
            ..entries[0]
        };
        assert!(push_address(&mut entries, excessive).is_err());
        assert_eq!(entries.len(), MAX_ADDRESSES);
    }

    #[test]
    fn enumerated_addresses_are_bounded_values_with_interface_indices() {
        let addresses = enumerate().unwrap();
        assert!(addresses.len() <= MAX_ADDRESSES);
        for entry in addresses {
            assert_ne!(entry.interface_index, 0);
            assert_eq!(entry.reserved, 0);
            assert_eq!(entry.address.reserved, 0);
            assert_eq!(entry.address.port, 0);
            assert_eq!(entry.address.flowinfo, 0);
            match entry.address.family {
                4 => assert_eq!(&entry.address.bytes[4..], &[0; 12]),
                6 => (),
                _ => panic!("unexpected native address family"),
            }
        }
    }
}
