use super::{
    Channel, Hub,
    abi::{ChannelId, Description, SINK, SOURCE},
};
use amaterasu_abi::{STATUS_INVALID_ARGUMENT, Status};

/// Two transfer channels owned by one authorized connection. The connection
/// owner must revoke the pair on disconnect, timeout, failed handoff or loss of
/// authorization. This type does not authenticate or own the connection.
///
/// SOURCE carries connection input to the native consumer; SINK carries its
/// output to the connection owner. Each channel's finish is a directional EOF;
/// finish drains accepted records and does not imply remote delivery. Dropping
/// the pair signals both channels, then synchronously retires them and releases
/// their budgets. Do not drop it from either channel's receive callback: the
/// retirement barrier waits for that callback. Before dropping, fence socket
/// effects and unblock and join workers that can wait on external operations.
pub struct DuplexPair {
    hub: Hub,
    input_id: ChannelId,
    output_id: ChannelId,
    input: Channel,
    output: Channel,
}

impl DuplexPair {
    /// Create after authorizing the exact connection, consumer and schemas.
    /// Nothing is exposed if either reservation fails. Both channels use the
    /// same runtime-attested native principal, not a client-supplied identity.
    pub fn create(
        hub: &Hub,
        owner: &str,
        input: Description,
        output: Description,
    ) -> Result<Self, Status> {
        if input.direction != SOURCE || output.direction != SINK {
            return Err(STATUS_INVALID_ARGUMENT);
        }
        let (input_id, input) = hub.create(owner, input)?;
        let (output_id, output) = match hub.create(owner, output) {
            Ok(pair) => pair,
            Err(error) => {
                hub.close(input_id);
                return Err(error);
            }
        };
        Ok(Self {
            hub: hub.clone(),
            input_id,
            output_id,
            input,
            output,
        })
    }

    /// Signal both directions without waiting for retirement. Connection owners
    /// fence socket effects and unblock external waits first, then signal before
    /// joining workers that may be waiting on these channels. Do not call from
    /// a receive callback if that callback can reenter channel cancellation.
    pub fn signal_cancel(&self) {
        self.hub.begin_close(self.input_id);
        self.hub.begin_close(self.output_id);
    }

    pub fn input_id(&self) -> ChannelId {
        self.input_id
    }
    pub fn output_id(&self) -> ChannelId {
        self.output_id
    }
    pub fn input(&self) -> &Channel {
        &self.input
    }
    pub fn output(&self) -> &Channel {
        &self.output
    }
}

impl Drop for DuplexPair {
    fn drop(&mut self) {
        self.signal_cancel();
        self.hub.close(self.input_id);
        self.hub.close(self.output_id);
    }
}
