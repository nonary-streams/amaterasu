use super::*;

impl Controller {
    pub(super) fn poll_caller(&mut self, runtime: &RuntimeClient, now: u64) -> ModuleResult<()> {
        match runtime.send_ephemeral(Command::Poll {})? {
            Response::Request {
                id,
                caller,
                service,
                payload,
                remaining_ms,
                ..
            } => {
                if service != crate::SERVICE {
                    rpc::complete(runtime, id, error("invalid_service"));
                } else if !rpc::active(runtime, id) {
                    // The broker has already cancelled the caller-owned ticket.
                } else {
                    match serde_json::from_value::<Operation>(payload) {
                        Ok(operation) => {
                            self.receive(id, caller, operation, now, remaining_ms, runtime)
                        }
                        Err(_) => {
                            rpc::complete(runtime, id, error("invalid_request"));
                        }
                    }
                }
                Ok(())
            }
            Response::Pending => Ok(()),
            _ => Err("RTSP poll failed".into()),
        }
    }

    pub(super) fn receive(
        &mut self,
        request: u64,
        caller: String,
        operation: Operation,
        now: u64,
        remaining: u64,
        runtime: &RuntimeClient,
    ) {
        match operation {
            Operation::Open(config) => self.open(request, caller, config, now, remaining, runtime),
            Operation::NetworkConfigure {
                port,
                address_family,
                bind_address,
            } => {
                if caller != "amaterasu.nvhttp" {
                    rpc::complete(runtime, request, error("permission_denied"));
                } else {
                    let payload = self
                        .configure_network_policy(port, address_family, bind_address, now)
                        .unwrap_or_else(error);
                    rpc::complete(runtime, request, payload);
                }
            }
            Operation::Close { ticket } => self.close(request, &caller, ticket, now, runtime),
            Operation::Status { ticket } => self.status(request, &caller, ticket, runtime),
            Operation::Lookup { operation_id } => {
                self.lookup(request, &caller, &operation_id, runtime)
            }
        }
    }

    pub(super) fn open(
        &mut self,
        request: u64,
        caller: String,
        config: Open,
        now: u64,
        remaining: u64,
        runtime: &RuntimeClient,
    ) {
        if !self.settings.enabled {
            rpc::complete(runtime, request, error("disabled"));
            return;
        }
        if !self
            .settings
            .allowed_principals
            .iter()
            .any(|allowed| allowed == &caller)
        {
            rpc::complete(runtime, request, error("permission_denied"));
            return;
        }
        if let Err(code) = config.validate(self.settings.allow_remote) {
            rpc::complete(runtime, request, error(code));
            return;
        }
        let local = match self.advertised_local_for_peer(config.peer_ip) {
            Ok(local) => local,
            Err(code) => {
                rpc::complete(runtime, request, error(code));
                return;
            }
        };
        if let Some(existing) = self.records.values().find(|record| {
            record.owner == caller && record.config.operation_id == config.operation_id
        }) {
            let response = if existing.config == config {
                existing.response()
            } else {
                error("operation_conflict")
            };
            rpc::complete(runtime, request, response);
            return;
        }
        if self.records.len() >= MAX_RECORDS
            || self
                .records
                .values()
                .filter(|record| !record.state.terminal())
                .count()
                >= MAX_LIVE_TICKETS
        {
            rpc::complete(runtime, request, error("capacity_exceeded"));
            return;
        }
        let (ticket, session_token, udp_discovery) = match self.new_identity(config.discover_peers)
        {
            Ok(identity) => identity,
            Err(code) => {
                rpc::complete(runtime, request, error(code));
                return;
            }
        };
        let url = format!("rtspenc://{local}/launch-{ticket:016x}");
        let expires = now.saturating_add(remaining.min(PREPARE_BUDGET_MS));
        let record = Record {
            id: ticket,
            owner: caller,
            config,
            state: State::Preparing,
            session: None,
            negotiation: None,
            url,
            session_token,
            udp_discovery,
            local,
            expires,
            next_status: expires,
            failure: None,
            cleanup_error: None,
            uncertain: None,
            uncertain_phase: None,
            needs_lookup: true,
            stream_state: None,
            revoke_after: None,
            cleanup_confirmed: false,
        };
        let media_binds = self.media_binds_for_open(local.ip());
        let payload = record.config.prepare_with_media_binds(
            &record.operation(),
            record.udp_discovery.as_ref(),
            media_binds,
        );
        let operation = self.operation_id(&record, "prepare");
        self.records.insert(ticket, record);
        self.job = Some(Job {
            ticket,
            phase: Phase::Prepare,
            call: Call::new(payload, operation, Some(request)),
            caller: Some(request),
            network: None,
            deadline: expires,
        });
    }

    pub(super) fn close(
        &mut self,
        request: u64,
        caller: &str,
        ticket: u64,
        now: u64,
        runtime: &RuntimeClient,
    ) {
        let Some(mut record) = self.records.remove(&ticket) else {
            rpc::complete(runtime, request, error("ticket_not_found"));
            return;
        };
        if record.owner != caller {
            self.records.insert(ticket, record);
            rpc::complete(runtime, request, error("ticket_not_found"));
            return;
        }
        if record.state.terminal() {
            rpc::complete(runtime, request, record.response());
            self.records.insert(ticket, record);
            return;
        }
        record.close(None);
        record.cleanup_error = None;
        self.revoke_ticket(ticket);
        self.schedule_cleanup(&mut record, Some(request), now);
        if self.job.is_none() {
            complete_caller(&record, Some(request), runtime);
        }
        self.records.insert(ticket, record);
    }

    pub(super) fn status(
        &self,
        request: u64,
        caller: &str,
        ticket: Option<u64>,
        runtime: &RuntimeClient,
    ) {
        let response = if let Some(ticket) = ticket {
            self.records
                .get(&ticket)
                .filter(|record| record.owner == caller)
                .map(Record::response)
                .unwrap_or_else(|| error("ticket_not_found"))
        } else {
            let owned: Vec<_> = self
                .records
                .values()
                .filter(|record| record.owner == caller)
                .collect();
            let tickets: Vec<_> = owned
                .iter()
                .filter(|record| !record.state.terminal())
                .chain(owned.iter().filter(|record| record.state.terminal()))
                .take(16)
                .map(|record| record.response())
                .collect();
            json!({"ok":true,"enabled":self.settings.enabled,"local":self.local,
                "locals":self.locals,
                "connections":self.transport.as_ref().map_or(0, Transport::connection_count),
                "tickets":tickets,"total":owned.len(),"truncated":owned.len()>16})
        };
        rpc::complete(runtime, request, response);
    }

    pub(super) fn lookup(
        &self,
        request: u64,
        caller: &str,
        operation_id: &str,
        runtime: &RuntimeClient,
    ) {
        let response = if valid_name(operation_id) {
            self.records
                .values()
                .find(|record| record.owner == caller && record.config.operation_id == operation_id)
                .map(Record::response)
                .unwrap_or_else(|| error("ticket_not_found"))
        } else {
            error("invalid_operation_id")
        };
        rpc::complete(runtime, request, response);
    }

    pub(super) fn new_identity(
        &self,
        discover_peers: bool,
    ) -> Result<(u64, String, Option<UdpDiscovery>), &'static str> {
        for _ in 0..8 {
            let mut bytes = [0; 48];
            getrandom::fill(&mut bytes).map_err(|_| "random_unavailable")?;
            let id = u64::from_ne_bytes(bytes[..8].try_into().expect("fixed slice"));
            if id == 0 || self.records.contains_key(&id) {
                continue;
            }
            let mut token = String::with_capacity(32);
            for byte in &bytes[8..24] {
                use std::fmt::Write;
                write!(&mut token, "{byte:02x}").expect("string write");
            }
            let udp_discovery = discover_peers.then(|| UdpDiscovery {
                audio_ping_payload: encode_ping_payload(
                    bytes[24..36].try_into().expect("fixed slice"),
                ),
                video_ping_payload: encode_ping_payload(
                    bytes[36..48].try_into().expect("fixed slice"),
                ),
            });
            if udp_discovery.as_ref().is_some_and(|discovery| {
                discovery.audio_ping_payload == discovery.video_ping_payload
            }) {
                continue;
            }
            return Ok((id, token, udp_discovery));
        }
        Err("identifier_exhausted")
    }
}

fn encode_ping_payload(bytes: &[u8; 12]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(16);
    for chunk in bytes.as_chunks::<3>().0 {
        let bits = u32::from_be_bytes([0, chunk[0], chunk[1], chunk[2]]);
        for shift in [18, 12, 6, 0] {
            output.push(ALPHABET[((bits >> shift) & 0x3f) as usize] as char);
        }
    }
    output
}
