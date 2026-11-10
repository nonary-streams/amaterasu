//! Principals that trusted composition stacks on a service's provider.
//!
//! Policy lists a service's layers outermost first. A layer registers for the
//! service like a provider does and receives the calls made to it. Its own
//! call to that service continues to the layer beneath it, and the innermost
//! layer's call reaches the provider. Consumers and the provider keep using
//! the one service name.
use amaterasu_contracts::Response;
use std::collections::{BTreeMap, BTreeSet};

/// Who serves a call, relative to the caller's place in the stack.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum NextHop<'a> {
    Layer(&'a str),
    Provider,
}

#[derive(Default)]
pub(super) struct ServiceLayers {
    configured: BTreeMap<String, Vec<String>>,
    registered: BTreeMap<String, BTreeSet<String>>,
}

impl ServiceLayers {
    pub fn new(configured: BTreeMap<String, Vec<String>>) -> Self {
        Self {
            configured,
            registered: BTreeMap::new(),
        }
    }

    pub fn is_layer(&self, service: &str, principal: &str) -> bool {
        self.position(service, principal).is_some()
    }

    /// Records that a configured layer is ready to receive calls. Returns
    /// whether `principal` is a layer of `service` at all.
    pub fn register(&mut self, service: &str, principal: &str) -> bool {
        if !self.is_layer(service, principal) {
            return false;
        }
        self.registered
            .entry(service.to_owned())
            .or_default()
            .insert(principal.to_owned());
        true
    }

    /// The hop that serves `caller`'s call to `service`.
    ///
    /// Every layer still ahead of the caller must have registered. A call is
    /// refused at admission rather than queued into a stack that cannot carry
    /// it, and an absent layer is never skipped.
    pub fn next_hop(&self, service: &str, caller: &str) -> Result<NextHop<'_>, Response> {
        let Some(layers) = self.configured.get(service) else {
            return Ok(NextHop::Provider);
        };
        let ahead = match self.position(service, caller) {
            // A layer's own call continues beneath it.
            Some(position) => &layers[position + 1..],
            // Every other caller enters at the outermost layer.
            None => layers.as_slice(),
        };
        let registered = self.registered.get(service);
        if ahead
            .iter()
            .any(|layer| !registered.is_some_and(|ready| ready.contains(layer)))
        {
            return Err(Response::error(
                "unavailable",
                "a layer of this service is not registered",
            ));
        }
        Ok(match ahead.first() {
            Some(layer) => NextHop::Layer(layer),
            None => NextHop::Provider,
        })
    }

    fn position(&self, service: &str, principal: &str) -> Option<usize> {
        self.configured
            .get(service)?
            .iter()
            .position(|layer| layer == principal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack() -> ServiceLayers {
        ServiceLayers::new(
            [(
                "example.echo/1".to_owned(),
                vec!["outer".to_owned(), "inner".to_owned()],
            )]
            .into(),
        )
    }

    fn unavailable(result: Result<NextHop<'_>, Response>) -> bool {
        matches!(result, Err(Response::Error { code, .. }) if code == "unavailable")
    }

    #[test]
    fn an_unlayered_service_goes_straight_to_its_provider() {
        let layers = stack();
        assert_eq!(
            layers.next_hop("example.other/1", "consumer").unwrap(),
            NextHop::Provider
        );
        assert!(!layers.is_layer("example.other/1", "outer"));
    }

    #[test]
    fn each_caller_reaches_the_layer_beneath_it() {
        let mut layers = stack();
        assert!(layers.register("example.echo/1", "outer"));
        assert!(layers.register("example.echo/1", "inner"));
        assert_eq!(
            layers.next_hop("example.echo/1", "consumer").unwrap(),
            NextHop::Layer("outer")
        );
        assert_eq!(
            layers.next_hop("example.echo/1", "outer").unwrap(),
            NextHop::Layer("inner")
        );
        assert_eq!(
            layers.next_hop("example.echo/1", "inner").unwrap(),
            NextHop::Provider
        );
    }

    #[test]
    fn an_absent_layer_is_never_skipped() {
        let mut layers = stack();
        assert!(layers.register("example.echo/1", "outer"));
        // The consumer and the outer layer both still need the inner layer.
        assert!(unavailable(layers.next_hop("example.echo/1", "consumer")));
        assert!(unavailable(layers.next_hop("example.echo/1", "outer")));
        // Nothing lies between the inner layer and the provider.
        assert_eq!(
            layers.next_hop("example.echo/1", "inner").unwrap(),
            NextHop::Provider
        );
    }

    #[test]
    fn only_a_configured_layer_can_register() {
        let mut layers = stack();
        assert!(!layers.register("example.echo/1", "stranger"));
        assert!(!layers.register("example.other/1", "outer"));
        assert!(unavailable(layers.next_hop("example.echo/1", "consumer")));
    }
}
