use super::*;
use amaterasu_module_sdk::session_presentation::{Client, Game};

fn context() -> Context {
    Context {
        client: Client {
            id: "authenticated-client".into(),
            paired: true,
            may_list: true,
            may_view: true,
            may_launch: true,
            may_terminate: false,
        },
        game: None,
    }
}

fn application(id: u32) -> Application {
    Application {
        id: format!("app-{id:016x}"),
        uuid: format!("configured-{id}"),
        title: format!("Game {id}"),
        aliases: Vec::new(),
        visible: true,
        monitor_profile: false,
        properties: json!({}),
    }
}

fn inventory(revision: u64, entries: Vec<Application>, after: Option<u32>) -> Value {
    json!({
        "ok":true,
        "revision":revision,
        "entries":entries,
        "next_cursor":after.map(|id| json!({
            "revision":revision,"after":format!("app-{id:016x}"),"query":""
        }))
    })
}

fn provider(reply: session::Reply) -> Value {
    serde_json::to_value(reply).unwrap()
}

fn presentation() -> Presentation {
    Presentation {
        busy: false,
        current_game: None,
        current_game_uuid: None,
    }
}

fn provider_request(list: &CatalogProjection) -> Request {
    match list.next_call() {
        ProjectionCall::Provider(request) => *request,
        ProjectionCall::Library(_) => panic!("expected provider request"),
    }
}

fn begin(list: &mut CatalogProjection) {
    assert!(matches!(
        provider_request(list),
        Request::BeginProject { .. }
    ));
    assert!(
        list.accept(provider(session::Reply::Started {
            projection_id: "projection-1".into(),
        }))
        .unwrap()
        .is_none()
    );
}

fn upload_all(list: &mut CatalogProjection) {
    loop {
        let final_page = match provider_request(list) {
            Request::AppendProject { final_page, .. } => final_page,
            _ => panic!("expected append"),
        };
        assert!(
            list.accept(provider(session::Reply::Appended {
                complete: final_page
            }))
            .unwrap()
            .is_none()
        );
        if final_page {
            break;
        }
    }
}

#[test]
fn complete_inventory_and_release_are_required_before_one_xml_reply() {
    let mut list = CatalogProjection::new(context(), false, true);
    match list.next_call() {
        ProjectionCall::Library(payload) => {
            assert_eq!(payload, json!({"action":"identity_inventory","limit":32}));
        }
        ProjectionCall::Provider(_) => panic!("inventory must precede projection"),
    }
    assert!(
        list.accept(inventory(7, vec![application(1)], Some(1)))
            .unwrap()
            .is_none()
    );
    match list.next_call() {
        ProjectionCall::Library(payload) => {
            assert_eq!(payload["cursor"]["revision"], 7);
            assert_eq!(payload["cursor"]["after"], "app-0000000000000001");
        }
        ProjectionCall::Provider(_) => panic!("whole inventory must be collected first"),
    }
    assert!(
        list.accept(inventory(7, vec![application(2)], None))
            .unwrap()
            .is_none()
    );
    begin(&mut list);
    match provider_request(&list) {
        Request::AppendProject {
            entries,
            final_page,
            client_id,
            projection_id,
        } => {
            assert!(final_page);
            assert_eq!(client_id, "authenticated-client");
            assert_eq!(projection_id, "projection-1");
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.id.as_str())
                    .collect::<Vec<_>>(),
                ["1", "2"]
            );
        }
        _ => panic!("expected normalized inventory upload"),
    }
    upload_all(&mut list);
    let mut output = application(1);
    output.id = "1".into();
    output.title = "  <Retained & Game>".into();
    assert!(
        list.accept(provider(session::Reply::Projected {
            presentation: presentation(),
            entries: vec![output],
            next_offset: None,
        }))
        .unwrap()
        .is_none()
    );
    assert!(matches!(
        provider_request(&list),
        Request::ReleaseProject { .. }
    ));
    let reply = list
        .accept(provider(session::Reply::Acknowledged {}))
        .unwrap()
        .expect("reply follows release acknowledgement");
    assert_eq!(reply.status, 200);
    assert!(reply.requires_paired_tls);
    assert_eq!(reply.permission, Some(Permission::List));
    let body = String::from_utf8(reply.body).unwrap();
    assert!(body.contains("<AppTitle>  &lt;Retained &amp; Game&gt;</AppTitle>"));
    assert!(body.contains("<ID>1</ID>"));
    assert!(body.contains("<UUID>configured-1</UUID>"));
    assert!(body.contains("<IsHdrSupported>1</IsHdrSupported>"));
    assert!(body.ends_with(SUFFIX));
    assert!(list.release_request().is_none());
}

#[test]
fn hidden_ids_aliases_and_profile_properties_reach_provider_unchanged() {
    let mut list = CatalogProjection::new(context(), false, false);
    let mut hidden = application(5);
    hidden.visible = false;
    hidden.aliases = vec!["app-000000007fffff6b".into()];
    hidden.monitor_profile = true;
    hidden.properties = json!({"display":{"width":1920,"hdr":true}});
    list.accept(inventory(4, vec![hidden.clone()], None))
        .unwrap();
    begin(&mut list);
    match provider_request(&list) {
        Request::AppendProject { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, "5");
            assert_eq!(entries[0].aliases, ["2147483499"]);
            assert!(!entries[0].visible);
            assert!(entries[0].monitor_profile);
            assert_eq!(entries[0].properties, hidden.properties);
        }
        _ => panic!("expected inventory append"),
    }
}

#[test]
fn active_game_metadata_is_enriched_before_provider_context_is_frozen() {
    let mut facts = context();
    let mut observed = application(2);
    observed.id = "2".into();
    observed.title = "Application".into();
    observed.uuid.clear();
    facts.game = Some(Game {
        application: observed,
        owner_client_id: "owner".into(),
        generation: 9,
        source: None,
    });
    let mut list = CatalogProjection::new(facts, false, false);
    list.accept(inventory(7, vec![application(1)], Some(1)))
        .unwrap();
    let mut configured = application(2);
    configured.title = "Actual game".into();
    list.accept(inventory(7, vec![configured.clone()], None))
        .unwrap();
    match provider_request(&list) {
        Request::BeginProject {
            context,
            catalog_revision,
        } => {
            assert_eq!(context.game.unwrap().application.title, "Actual game");
            assert_eq!(catalog_revision, "7");
        }
        _ => panic!("expected project begin"),
    }
}

#[test]
fn large_profile_properties_split_uploads_below_the_actual_api_byte_limit() {
    let mut list = CatalogProjection::new(context(), false, false);
    for first_id in (1..=32).step_by(2) {
        let entries = (first_id..first_id + 2)
            .map(|id| {
                let mut entry = application(id);
                entry.properties = json!({"padding":"x".repeat(3_900)});
                entry
            })
            .collect();
        let page = inventory(1, entries, (first_id < 31).then_some(first_id + 1));
        assert!(serde_json::to_vec(&page).unwrap().len() < session::MAX_PAYLOAD_BYTES);
        list.accept(page).unwrap();
    }
    begin(&mut list);
    let mut uploaded = 0;
    let mut requests = 0;
    loop {
        let request = provider_request(&list);
        request.validate().unwrap();
        assert!(serde_json::to_vec(&request).unwrap().len() <= session::MAX_PAYLOAD_BYTES);
        let Request::AppendProject {
            entries,
            final_page,
            ..
        } = request
        else {
            panic!("expected bounded append");
        };
        assert!(!entries.is_empty());
        uploaded += entries.len();
        requests += 1;
        list.accept(provider(session::Reply::Appended {
            complete: final_page,
        }))
        .unwrap();
        if final_page {
            break;
        }
    }
    assert_eq!(uploaded, 32);
    assert!(requests > 1);
}

#[test]
fn empty_inventory_is_uploaded_as_one_final_page() {
    let mut list = CatalogProjection::new(context(), false, false);
    list.accept(inventory(0, vec![], None)).unwrap();
    begin(&mut list);
    match provider_request(&list) {
        Request::AppendProject {
            entries,
            final_page,
            ..
        } => {
            assert!(entries.is_empty());
            assert!(final_page);
        }
        _ => panic!("expected explicit final page"),
    }
}

#[test]
fn view_only_inventory_refresh_skips_public_reads_and_preserves_hidden_occupancy() {
    let mut facts = context();
    facts.client.may_list = false;
    facts.client.may_view = true;
    facts.client.may_launch = false;
    let mut observed = application(2);
    observed.id = "2".into();
    observed.title = "Application".into();
    facts.game = Some(Game {
        application: observed,
        owner_client_id: "owner".into(),
        generation: 9,
        source: None,
    });
    let mut list = CatalogProjection::inventory(facts, false, true);
    assert!(matches!(list.next_call(), ProjectionCall::Library(_)));
    let mut hidden = application(1);
    hidden.visible = false;
    hidden.aliases = vec!["app-000000000000002a".into()];
    hidden.properties = json!({"retained":"profile facts"});
    list.accept(inventory(7, vec![hidden], Some(1))).unwrap();
    assert!(matches!(list.next_call(), ProjectionCall::Library(_)));
    list.accept(inventory(7, vec![application(2)], None))
        .unwrap();
    match provider_request(&list) {
        Request::BeginProject { context, .. } => {
            assert!(!context.client.may_list);
            assert!(context.client.may_view);
            assert!(!context.client.may_launch);
            assert_eq!(context.game.unwrap().application.title, "Game 2");
        }
        _ => panic!("expected backend inventory transaction"),
    }
    begin(&mut list);
    match provider_request(&list) {
        Request::AppendProject {
            entries,
            final_page,
            ..
        } => {
            assert!(final_page);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].id, "1");
            assert!(!entries[0].visible);
            assert_eq!(entries[0].aliases, ["42"]);
            assert_eq!(entries[0].properties, json!({"retained":"profile facts"}));
        }
        _ => panic!("expected complete occupancy upload"),
    }
    // The only provider calls are Begin, Append and Release. ReadProject would
    // require independently granted List authority and must never occur here.
    upload_all(&mut list);
    assert!(matches!(
        provider_request(&list),
        Request::ReleaseProject { .. }
    ));
    assert!(list.release_request().is_some());
    let completion = list
        .accept(provider(session::Reply::Acknowledged {}))
        .unwrap()
        .expect("backend refresh completes only after release");
    assert_eq!(completion.status, 200);
    assert!(completion.body.is_empty());
    assert!(completion.requires_paired_tls);
    assert_eq!(completion.permission, None);
    assert!(list.release_request().is_none());
    assert!(!list.context().client.may_list);
    assert!(list.context().client.may_view);
}

#[test]
fn list_only_clients_keep_public_projection_without_stream_view_authority() {
    let mut facts = context();
    facts.client.may_list = true;
    facts.client.may_view = false;
    facts.client.may_launch = false;
    let mut list = CatalogProjection::new(facts, false, false);
    list.accept(inventory(1, vec![application(1)], None))
        .unwrap();
    match provider_request(&list) {
        Request::BeginProject { context, .. } => {
            assert!(context.client.may_list);
            assert!(!context.client.may_view);
        }
        _ => panic!("expected public projection transaction"),
    }
    begin(&mut list);
    upload_all(&mut list);
    assert!(matches!(
        provider_request(&list),
        Request::ReadProject { .. }
    ));
    assert!(
        list.accept(provider(session::Reply::Projected {
            presentation: presentation(),
            entries: vec![projected_application(1)],
            next_offset: None,
        }))
        .unwrap()
        .is_none()
    );
    assert!(matches!(
        provider_request(&list),
        Request::ReleaseProject { .. }
    ));
    let reply = list
        .accept(provider(session::Reply::Acknowledged {}))
        .unwrap()
        .unwrap();
    assert_eq!(reply.permission, Some(Permission::List));
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("<ID>1</ID>")
    );
}

#[test]
fn desktop_is_part_of_occupancy_and_projection_instead_of_appended_afterward() {
    let mut list = CatalogProjection::new(context(), true, false);
    list.accept(inventory(1, vec![], None)).unwrap();
    begin(&mut list);
    match provider_request(&list) {
        Request::AppendProject { entries, .. } => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, DESKTOP_APP_ID.to_string());
            assert_eq!(entries[0].title, "Desktop");
        }
        _ => panic!("expected Desktop identity upload"),
    }
    for alias_collision in [false, true] {
        let mut list = CatalogProjection::new(context(), true, false);
        let mut entry = application(if alias_collision { 1 } else { DESKTOP_APP_ID });
        if alias_collision {
            entry.aliases.push(format!("app-{DESKTOP_APP_ID:016x}"));
        }
        assert_eq!(
            list.accept(inventory(1, vec![entry], None))
                .unwrap_err()
                .status,
            502
        );
    }
}

#[test]
fn inconsistent_inventory_revision_cursor_and_identity_fail_before_projection() {
    let cases = [
        inventory(2, vec![application(2)], Some(1)),
        inventory(2, vec![], Some(1)),
        inventory(2, vec![application(1), application(1)], None),
        inventory(2, vec![application(0)], None),
        inventory(2, vec![application(i32::MAX as u32 + 1)], None),
    ];
    for page in cases {
        let mut list = CatalogProjection::new(context(), false, false);
        assert_eq!(list.accept(page).unwrap_err().status, 502);
        assert!(list.release_request().is_none());
    }
    let mut list = CatalogProjection::new(context(), false, false);
    list.accept(inventory(2, vec![application(1)], Some(1)))
        .unwrap();
    assert_eq!(
        list.accept(inventory(3, vec![application(2)], None))
            .unwrap_err()
            .status,
        409
    );
    assert!(list.release_request().is_none());
}

#[test]
fn corrupt_hidden_aliases_and_oversized_properties_fail_before_policy_admission() {
    for entry in [
        Application {
            aliases: vec!["1".into()],
            ..application(1)
        },
        Application {
            properties: json!({"padding":"x".repeat(4096)}),
            ..application(1)
        },
        Application {
            title: "\ninvalid".into(),
            ..application(1)
        },
    ] {
        let mut list = CatalogProjection::new(context(), false, false);
        assert_eq!(
            list.accept(inventory(2, vec![entry], None))
                .unwrap_err()
                .status,
            502
        );
    }
}

fn ready_to_read() -> CatalogProjection {
    let mut list = CatalogProjection::new(context(), false, false);
    list.accept(inventory(2, vec![application(1)], None))
        .unwrap();
    begin(&mut list);
    upload_all(&mut list);
    list
}

fn projected_application(id: u32) -> Application {
    Application {
        id: id.to_string(),
        ..application(id)
    }
}

#[test]
fn projected_order_is_provider_owned_but_duplicates_and_cursor_loops_are_rejected() {
    let mut list = ready_to_read();
    list.accept(provider(session::Reply::Projected {
        presentation: presentation(),
        entries: vec![projected_application(20), projected_application(1)],
        next_offset: Some(2),
    }))
    .unwrap();
    assert!(matches!(
        provider_request(&list),
        Request::ReadProject { offset: 2, .. }
    ));
    assert_eq!(
        list.accept(provider(session::Reply::Projected {
            presentation: presentation(),
            entries: vec![projected_application(20)],
            next_offset: None,
        }))
        .unwrap_err()
        .status,
        502
    );
    assert!(list.release_request().is_some());
    for (entries, next_offset) in [(vec![], Some(1)), (vec![projected_application(1)], Some(0))] {
        let mut list = ready_to_read();
        assert_eq!(
            list.accept(provider(session::Reply::Projected {
                presentation: presentation(),
                entries,
                next_offset
            }))
            .unwrap_err()
            .status,
            502
        );
    }
}

#[test]
fn hidden_noncanonical_and_out_of_range_projected_entries_never_escape_as_xml() {
    for entry in [
        Application {
            visible: false,
            ..projected_application(1)
        },
        Application {
            id: "01".into(),
            ..projected_application(1)
        },
        Application {
            id: "app-0000000000000001".into(),
            ..projected_application(1)
        },
        projected_application(0),
        projected_application(i32::MAX as u32 + 1),
    ] {
        let mut list = ready_to_read();
        assert_eq!(
            list.accept(provider(session::Reply::Projected {
                presentation: presentation(),
                entries: vec![entry],
                next_offset: None,
            }))
            .unwrap_err()
            .status,
            502
        );
        assert!(list.release_request().is_some());
    }
}

#[test]
fn catalog_presentation_stays_frozen_across_read_pages() {
    let mut list = ready_to_read();
    list.accept(provider(session::Reply::Projected {
        presentation: presentation(),
        entries: vec![projected_application(1)],
        next_offset: Some(1),
    }))
    .unwrap();
    assert_eq!(
        list.accept(provider(session::Reply::Projected {
            presentation: Presentation {
                busy: true,
                current_game: Some("1".into()),
                current_game_uuid: None
            },
            entries: vec![],
            next_offset: None,
        }))
        .unwrap_err()
        .status,
        409
    );
    assert!(list.release_request().is_some());
}

#[test]
fn provider_errors_and_premature_completion_keep_transaction_releasable() {
    let mut list = ready_to_read();
    assert_eq!(
        list.accept(provider(session::Reply::Error {
            code: "temporary".into(),
            message: "details not forwarded".into(),
        }))
        .unwrap_err()
        .status,
        503
    );
    assert!(list.release_request().is_some());
    let mut list = CatalogProjection::new(context(), false, false);
    list.accept(inventory(2, vec![application(1)], None))
        .unwrap();
    begin(&mut list);
    assert_eq!(
        list.accept(provider(session::Reply::Appended { complete: false }))
            .unwrap_err()
            .status,
        502
    );
    assert!(list.release_request().is_some());
}

#[test]
fn release_failure_cannot_publish_the_prepared_list() {
    let mut list = ready_to_read();
    list.accept(provider(session::Reply::Projected {
        presentation: presentation(),
        entries: vec![projected_application(1)],
        next_offset: None,
    }))
    .unwrap();
    assert_eq!(
        list.accept(provider(session::Reply::Error {
            code: "unavailable".into(),
            message: "retry".into(),
        }))
        .unwrap_err()
        .status,
        503
    );
    assert!(list.release_request().is_some());
    assert_eq!(
        list.accept(provider(session::Reply::Started {
            projection_id: "other".into()
        }))
        .unwrap_err()
        .status,
        502
    );
}

#[test]
fn provider_hdr_restrictions_preserve_the_selected_encoder_limit() {
    for host_hdr in [false, true] {
        for row_hdr in [None, Some(false), Some(true)] {
            let mut list = CatalogProjection::new(context(), false, host_hdr);
            list.accept(inventory(1, vec![], None)).unwrap();
            begin(&mut list);
            upload_all(&mut list);
            let mut entry = projected_application(1);
            if let Some(supported) = row_hdr {
                entry.properties["hdr_supported"] = json!(supported);
            }
            list.accept(provider(session::Reply::Projected {
                presentation: presentation(),
                entries: vec![entry],
                next_offset: None,
            }))
            .unwrap();
            let reply = list
                .accept(provider(session::Reply::Acknowledged {}))
                .unwrap()
                .unwrap();
            let hdr = u8::from(host_hdr && row_hdr.unwrap_or(true));
            assert!(
                String::from_utf8(reply.body)
                    .unwrap()
                    .contains(&format!("<IsHdrSupported>{hdr}</IsHdrSupported>"))
            );
        }
    }
    let mut list = ready_to_read();
    let mut invalid = projected_application(1);
    invalid.properties["hdr_supported"] = json!("false");
    assert_eq!(
        list.accept(provider(session::Reply::Projected {
            presentation: presentation(),
            entries: vec![invalid],
            next_offset: None,
        }))
        .unwrap_err()
        .status,
        502
    );
    assert!(list.release_request().is_some());
}

#[test]
fn reply_limit_is_checked_after_xml_escaping() {
    let mut list = ready_to_read();
    let mut status = None;
    for page in 0..MAX_PAGES {
        let offset = page * PAGE_SIZE;
        let entries = (0..PAGE_SIZE)
            .map(|index| {
                let mut entry = projected_application((offset + index + 1) as u32);
                entry.title = "&".repeat(160);
                entry.uuid.clear();
                entry
            })
            .collect();
        if let Err(reply) = list.accept(provider(session::Reply::Projected {
            presentation: presentation(),
            entries,
            next_offset: Some(offset + PAGE_SIZE),
        })) {
            status = Some(reply.status);
            break;
        }
    }
    assert_eq!(status, Some(413));
    assert!(list.release_request().is_some());
}
