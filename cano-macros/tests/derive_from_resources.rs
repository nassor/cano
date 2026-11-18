//! Tests for `#[derive(FromResources)]`: plain fields resolve by type,
//! `#[res("name")]` fields resolve by name.

use cano::{Blackboard, Resources};
use cano_macros::{FromResources, Resource as ResourceDerive};
use std::sync::Arc;

#[derive(ResourceDerive)]
struct Counter {
    n: u32,
}

#[derive(FromResources)]
struct TypedDeps {
    store: Arc<Blackboard>,
    counter: Arc<Counter>,
}

#[test]
fn fields_resolve_by_type() {
    let res = Resources::new()
        .insert(Blackboard::new())
        .insert(Counter { n: 7 });
    let deps = TypedDeps::from_resources(&res).expect("should resolve");
    assert_eq!(deps.counter.n, 7);
    deps.store.put("k", 1u32);
    // The Arc is the instance stored in `res`.
    assert_eq!(res.get::<Blackboard>().unwrap().get::<u32>("k").unwrap(), 1);
}

#[derive(FromResources)]
struct NamedDeps {
    primary: Arc<Blackboard>,
    #[res("audit")]
    audit: Arc<Blackboard>,
}

#[test]
fn named_field_resolves_the_named_instance() {
    let res = Resources::new()
        .insert(Blackboard::new())
        .insert_named("audit", Blackboard::new());
    let deps = NamedDeps::from_resources(&res).expect("should resolve");
    deps.audit.put("who", "me".to_string());
    assert!(deps.primary.get::<String>("who").is_err());
    assert!(
        res.get_named::<Blackboard>("audit")
            .unwrap()
            .get::<String>("who")
            .is_ok()
    );
}

#[test]
fn missing_resource_is_an_error() {
    assert!(TypedDeps::from_resources(&Resources::new().insert(Blackboard::new())).is_err());
    let unnamed_only = Resources::new().insert(Blackboard::new());
    assert!(NamedDeps::from_resources(&unnamed_only).is_err());
}
