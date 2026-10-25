use crate::model::{
    GamepadFeedback, HeldInput, InputEvent, InputPacket, PROTOCOL_VERSION, PeerIdentity,
};
use crate::{InputError, InputResult};
use amaterasu_storage::{
    create_machine_private_directory, protect_machine_private_file, read_json,
    write_json_atomically,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_HELD: usize = 128;
const MAX_EVENTS_PER_SECOND: usize = 2048;
const HOST_AUTHORIZED_SESSION_WINDOW: Duration = Duration::from_secs(60 * 60);

/// A companion-owned device handle. `release_all` must neutralize the state
/// owned by that handle, independently of the replaceable worker's cleanup.
pub trait DeviceSink {
    fn submit(&mut self, event: &InputEvent) -> InputResult<()>;
    fn release_all(&mut self) -> InputResult<()>;
    fn keep_alive(&mut self) -> InputResult<()> {
        Ok(())
    }
    fn poll_gamepad_feedback(&mut self) -> InputResult<Option<GamepadFeedback>> {
        Ok(None)
    }
}

/// A single-use grant issued only after independent host policy approval.
/// Native publisher signatures alone cannot authorize this grant.
pub struct InputGrant {
    peer: PeerIdentity,
    lease_token: String,
    generation: u64,
    expires_at: Instant,
    keyboard: bool,
    mouse: bool,
    gamepad: bool,
    registry: Arc<Mutex<RegistryState>>,
}

#[derive(Default)]
struct RegistryState {
    // One exclusive device has one global replay floor across all users and
    // component sets. Upgrades cannot exhaust a table of historical digests.
    highest_generation: u64,
    // One source device is owned by at most one worker lease at a time.
    active: Option<(PeerIdentity, u64)>,
}

/// Generation floors are in memory for synthetic tests and persisted in a
/// protected machine directory by the installed service.
pub struct GrantRegistry {
    state: Arc<Mutex<RegistryState>>,
    generation_file: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedGenerations {
    schema: u32,
    highest_generation: u64,
}

impl GrantRegistry {
    // Tests use an isolated in-memory registry without changing machine trust.
    #[cfg(test)]
    pub(crate) fn synthetic() -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState::default())),
            generation_file: None,
        }
    }

    /// Creates an in-memory generation floor for an unprivileged user-session
    /// companion. Linux live grants are bound to a live host permit and random
    /// token; restarting the listener drops every grant, so persistent
    /// machine-owned storage is neither needed nor appropriate here.
    #[cfg(target_os = "linux")]
    pub fn session_only() -> Self {
        Self {
            state: Arc::new(Mutex::new(RegistryState::default())),
            generation_file: None,
        }
    }

    /// The installed service must supply an installer-owned machine root and
    /// run as LocalSystem. Generation floors survive a companion restart;
    /// live grants deliberately do not, so a restart invalidates all tickets.
    pub fn open_protected(root: &Path) -> InputResult<Self> {
        create_machine_private_directory(root).map_err(|_| InputError::Storage)?;
        let generation_file = root.join("input-generation-floors.json");
        let highest_generation = if generation_file
            .try_exists()
            .map_err(|_| InputError::Storage)?
        {
            let stored: PersistedGenerations =
                read_json(&generation_file).map_err(|_| InputError::Storage)?;
            if stored.schema != 1 {
                return Err(InputError::Storage);
            }
            protect_machine_private_file(&generation_file).map_err(|_| InputError::Storage)?;
            stored.highest_generation
        } else {
            0
        };
        Ok(Self {
            state: Arc::new(Mutex::new(RegistryState {
                highest_generation,
                active: None,
            })),
            generation_file: Some(generation_file),
        })
    }

    pub fn issue_next(
        &mut self,
        peer: PeerIdentity,
        lease_token: String,
        expires_at: Instant,
        keyboard: bool,
        mouse: bool,
    ) -> InputResult<(u64, InputGrant)> {
        self.issue_next_with_capabilities(peer, lease_token, expires_at, keyboard, mouse, false)
    }

    pub fn issue_next_with_capabilities(
        &mut self,
        peer: PeerIdentity,
        lease_token: String,
        expires_at: Instant,
        keyboard: bool,
        mouse: bool,
        gamepad: bool,
    ) -> InputResult<(u64, InputGrant)> {
        let next = {
            let state = self.state.lock().map_err(|_| InputError::InvalidGrant)?;
            state
                .highest_generation
                .checked_add(1)
                .ok_or(InputError::InvalidSequence)?
        };
        let grant = self.issue_with_capabilities(
            peer,
            lease_token,
            next,
            expires_at,
            keyboard,
            mouse,
            gamepad,
        )?;
        Ok((next, grant))
    }

    pub fn issue(
        &mut self,
        peer: PeerIdentity,
        lease_token: String,
        generation: u64,
        expires_at: Instant,
        keyboard: bool,
        mouse: bool,
    ) -> InputResult<InputGrant> {
        self.issue_with_capabilities(
            peer,
            lease_token,
            generation,
            expires_at,
            keyboard,
            mouse,
            false,
        )
    }

    // Preserve the explicit capability parameters of the public grant API.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_with_capabilities(
        &mut self,
        peer: PeerIdentity,
        lease_token: String,
        generation: u64,
        expires_at: Instant,
        keyboard: bool,
        mouse: bool,
        gamepad: bool,
    ) -> InputResult<InputGrant> {
        if lease_token.len() != 32
            || !lease_token.bytes().all(|byte| byte.is_ascii_hexdigit())
            || generation == 0
            || !keyboard && !mouse && !gamepad
        {
            return Err(InputError::InvalidGrant);
        }
        let mut registry = self.state.lock().map_err(|_| InputError::InvalidGrant)?;
        if registry.active.is_some() {
            return Err(InputError::ResourceExhausted);
        }
        if generation <= registry.highest_generation {
            return Err(InputError::InvalidSequence);
        }
        if let Some(path) = &self.generation_file {
            let record = PersistedGenerations {
                schema: 1,
                highest_generation: generation,
            };
            write_json_atomically(path, &record).map_err(|_| InputError::Storage)?;
            protect_machine_private_file(path).map_err(|_| InputError::Storage)?;
        }
        registry.highest_generation = generation;
        Ok(InputGrant {
            peer,
            lease_token,
            generation,
            expires_at,
            keyboard,
            mouse,
            gamepad,
            registry: Arc::clone(&self.state),
        })
    }
}

/// Companion-side validation before a restricted device interface is called.
pub struct InputSession<S: DeviceSink> {
    grant: InputGrant,
    pub(crate) sink: S,
    next_sequence: u64,
    held: Vec<HeldInput>,
    window_start: Instant,
    events_in_window: usize,
    closed: bool,
    cleanup_required: bool,
}

impl<S: DeviceSink> InputSession<S> {
    /// Reads one feedback event only for the current gamepad-enabled lease.
    /// Generation and token are checked independently of the Unix peer check.
    #[cfg(target_os = "linux")]
    pub(crate) fn poll_gamepad_feedback(
        &mut self,
        generation: u64,
        token: &str,
    ) -> InputResult<Option<GamepadFeedback>> {
        if self.closed || !self.grant.gamepad {
            return Err(InputError::PermissionDenied);
        }
        if generation != self.grant.generation || token != self.grant.lease_token {
            return Err(InputError::PermissionDenied);
        }
        let registry = self
            .grant
            .registry
            .lock()
            .map_err(|_| InputError::InvalidGrant)?;
        if registry.active.as_ref() != Some(&(self.grant.peer.clone(), self.grant.generation)) {
            return Err(InputError::PermissionDenied);
        }
        self.sink.poll_gamepad_feedback()
    }

    /// Called only while the separate host permit is current. This keeps a
    /// long-running authorized worker from hitting its original one-hour cap.
    pub(crate) fn refresh_host_authorized_expiry(&mut self, now: Instant) -> InputResult<()> {
        if self.closed {
            return Err(InputError::Closed);
        }
        self.grant.expires_at = now + HOST_AUTHORIZED_SESSION_WINDOW;
        Ok(())
    }
    pub fn connect(
        peer_from_transport: &PeerIdentity,
        grant: InputGrant,
        sink: S,
        now: Instant,
    ) -> InputResult<Self> {
        if peer_from_transport != &grant.peer {
            return Err(InputError::PermissionDenied);
        }
        if now >= grant.expires_at {
            return Err(InputError::Expired);
        }
        grant.activate()?;
        Ok(Self {
            grant,
            sink,
            next_sequence: 1,
            held: Vec::new(),
            window_start: now,
            events_in_window: 0,
            closed: false,
            cleanup_required: false,
        })
    }

    pub fn submit(&mut self, packet: &InputPacket, now: Instant) -> InputResult<()> {
        if self.closed {
            return Err(InputError::Closed);
        }
        if now >= self.grant.expires_at {
            let _ = self.revoke();
            return Err(InputError::Expired);
        }
        if packet.version != PROTOCOL_VERSION
            || packet.generation != self.grant.generation
            || packet.lease_token != self.grant.lease_token
        {
            return Err(InputError::PermissionDenied);
        }
        if packet.sequence != self.next_sequence {
            return Err(InputError::InvalidSequence);
        }
        packet.event.validate()?;
        if packet.event.keyboard() && !self.grant.keyboard
            || packet.event.gamepad() && !self.grant.gamepad
            || !packet.event.keyboard() && !packet.event.gamepad() && !self.grant.mouse
        {
            return Err(InputError::PermissionDenied);
        }
        let transition = packet.event.transition();
        self.validate_transition(&packet.event, transition)?;
        self.check_rate(now, transition)?;

        // Keep issuance of a newer generation serialized with device I/O.
        // Once `issue` returns, the old session cannot submit another event.
        let registry_handle = Arc::clone(&self.grant.registry);
        let registry = match registry_handle.lock() {
            Ok(registry) => registry,
            Err(_) => {
                let _ = self.revoke();
                return Err(InputError::InvalidGrant);
            }
        };
        if registry.active.as_ref() != Some(&(self.grant.peer.clone(), self.grant.generation)) {
            drop(registry);
            let _ = self.revoke();
            return Err(InputError::PermissionDenied);
        }

        // Consume the sequence before I/O: a failed acknowledgement might
        // follow a delivered event and must not be replayed.
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(InputError::InvalidSequence)?;
        if let Some((held, true)) = transition {
            self.held.push(held);
        }
        if let Err(error) = self.sink.submit(&packet.event) {
            drop(registry);
            self.cleanup_required = true;
            let _ = self.revoke();
            return Err(error);
        }
        drop(registry);
        if let Some((held, false)) = transition {
            self.held.retain(|item| *item != held);
        }
        Ok(())
    }

    fn validate_transition(
        &self,
        event: &InputEvent,
        transition: Option<(HeldInput, bool)>,
    ) -> InputResult<()> {
        if let InputEvent::EvdevKeyRepeat { code } = event
            && !self.held.contains(&HeldInput::EvdevKey(*code))
        {
            return Err(InputError::InvalidSequence);
        }
        match transition {
            Some((held, true)) if self.held.contains(&held) => Err(InputError::InvalidSequence),
            Some((_, true)) if self.held.len() >= MAX_HELD => Err(InputError::ResourceExhausted),
            Some((held, false)) if !self.held.contains(&held) => Err(InputError::InvalidSequence),
            _ => Ok(()),
        }
    }

    fn check_rate(
        &mut self,
        now: Instant,
        transition: Option<(HeldInput, bool)>,
    ) -> InputResult<()> {
        if now.saturating_duration_since(self.window_start) >= Duration::from_secs(1) {
            self.window_start = now;
            self.events_in_window = 0;
        }
        // Never rate-limit a release needed to clear a held state.
        if self.events_in_window >= MAX_EVENTS_PER_SECOND && !matches!(transition, Some((_, false)))
        {
            return Err(InputError::ResourceExhausted);
        }
        self.events_in_window = self.events_in_window.saturating_add(1);
        Ok(())
    }

    /// Call from the companion timer even when no worker packet arrives.
    /// Connection close and policy revocation must call `revoke` immediately.
    pub fn poll_expiry(&mut self, now: Instant) -> InputResult<bool> {
        if self.closed {
            if self.cleanup_required {
                self.revoke()?;
                return Ok(true);
            }
            return Ok(false);
        }
        if now < self.grant.expires_at {
            match self.grant.is_active() {
                Ok(true) => {
                    if let Err(error) = self.sink.keep_alive() {
                        let _ = self.revoke();
                        return Err(error);
                    }
                    return Ok(false);
                }
                Ok(false) => {}
                Err(error) => {
                    let _ = self.revoke();
                    return Err(error);
                }
            }
        }
        self.revoke()?;
        Ok(true)
    }

    pub fn revoke(&mut self) -> InputResult<()> {
        if self.closed && !self.cleanup_required {
            return Ok(());
        }
        self.closed = true;
        let mut failure = None;
        for index in (0..self.held.len()).rev() {
            match self.sink.submit(&self.held[index].release()) {
                Ok(()) => {
                    self.held.remove(index);
                }
                Err(error) => failure = Some(error),
            }
        }
        match self.sink.release_all() {
            Ok(()) => {
                self.held.clear();
                self.cleanup_required = false;
                self.grant.deactivate()?;
            }
            Err(error) => {
                self.cleanup_required = true;
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    pub fn cleanup_required(&self) -> bool {
        self.cleanup_required
    }
}

impl InputGrant {
    pub fn keyboard_enabled(&self) -> bool {
        self.keyboard
    }

    pub fn mouse_enabled(&self) -> bool {
        self.mouse
    }

    pub fn gamepad_enabled(&self) -> bool {
        self.gamepad
    }

    fn activate(&self) -> InputResult<()> {
        let mut registry = self.registry.lock().map_err(|_| InputError::InvalidGrant)?;
        if registry.highest_generation != self.generation {
            return Err(InputError::InvalidSequence);
        }
        if registry.active.is_some() {
            return Err(InputError::ResourceExhausted);
        }
        registry.active = Some((self.peer.clone(), self.generation));
        Ok(())
    }

    fn is_active(&self) -> InputResult<bool> {
        let registry = self.registry.lock().map_err(|_| InputError::InvalidGrant)?;
        Ok(registry.active.as_ref() == Some(&(self.peer.clone(), self.generation)))
    }

    fn deactivate(&self) -> InputResult<()> {
        let mut registry = self.registry.lock().map_err(|_| InputError::InvalidGrant)?;
        if registry.active.as_ref() == Some(&(self.peer.clone(), self.generation)) {
            registry.active = None;
        }
        Ok(())
    }
}

impl<S: DeviceSink> Drop for InputSession<S> {
    fn drop(&mut self) {
        if !self.closed || self.cleanup_required {
            let _ = self.revoke();
        }
    }
}
