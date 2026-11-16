use mugiurushi::{
    browser::{BrowserSpec, DatabaseRead, ResponseRecord, evaluate, query, validate},
    container::{ContainerPhase, ContainerSpec},
    trial::{CheckMethod, Expectation, Observed, TrialCheck},
};
use serde_json::json;

fn spec() -> BrowserSpec {
    serde_json::from_value(json!({
        "service":"web","tls":false,"start_path":"/","expected_version":"151.0.7922.34",
        "preparation":[{"id":"username","action":"fill","target":{"locator":{"by":"label","text":"利用者"}},"value":{"source":"secret","key":"username"}}],
        "actions":[{"id":"message","action":"read_text","target":{"locator":{"by":"role","role":"heading","name":"完了"}}}],
        "responses":[{"id":"save","method":"POST","path":"/save","contains":"saved"}],
        "database":[{"id":"row","service":"db","database":"trial","table":"patient","match_column":"name","columns":["name"]}]
    })).unwrap()
}
fn containers() -> ContainerSpec {
    serde_json::from_value(json!({"services":(["web","db"].map(|name| json!({"name":name,"image":format!("example@sha256:{}","a".repeat(64)),"role":if name=="db"{"database"}else{"app"},"expose":80,"env":{},"volumes":[],"command":[],"ready":"tcp","ready_timeout_ms":1000})))})).unwrap()
}
fn phase() -> ContainerPhase {
    serde_json::from_value(json!({"engine":"test","network":"test","volumes":[],"services":[],"leftovers":[],"database_query":null,"http":null,"detail":"合成の記録",
        "browser":{"schema_version":1,"run_id":"test","version":"151.0.7922.34","prepared":true,"completed":false,"steps":[{"id":"message","phase":"action","completed":true,"text":"完了","screenshot":null,"detail":"合成"}],"responses":[],"blocked_requests":0,"detail":"合成"},
        "database_reads":[]
    })).unwrap()
}
fn check(method: CheckMethod, expect: Expectation, target: &str) -> TrialCheck {
    TrialCheck {
        id: "test".into(),
        scope: "保存".into(),
        method,
        expect,
        text: Some("test".into()),
        target: Some(target.into()),
        relation: None,
    }
}
#[test]
fn validates_declarative_steps_and_rejects_remote_navigation_and_sql_identifiers() {
    let original = spec();
    let containers = containers();
    validate(&original, &containers).unwrap();
    let mut invalid = original.clone();
    invalid.start_path = "https://example.invalid/".into();
    assert!(validate(&invalid, &containers).is_err());
    invalid = original.clone();
    invalid.actions[0].id = "username".into();
    assert!(validate(&invalid, &containers).is_err());
    invalid = original.clone();
    invalid.database[0].table = "patient`; DROP TABLE patient;--".into();
    assert!(validate(&invalid, &containers).is_err());
    let sql = query(
        Default::default(),
        &original.database[0],
        "x'; DELETE FROM patient; --",
    );
    assert!(!sql.contains("DELETE"));
    assert!(sql.contains("CONVERT(0x"));
}
#[test]
fn schema_is_accepted_only_for_postgres_and_keeps_the_saved_form_without_it() {
    let original = spec();
    let mut mariadb = containers();
    // 指定しない設定の保存形は変わらない。
    assert!(
        !serde_json::to_string(&original.database[0])
            .unwrap()
            .contains("schema")
    );
    let mut with_schema = original.clone();
    with_schema.database[0].schema = Some("clinic".into());
    // MariaDBでは `database` がスキーマにあたるので、指定を拒否する。
    let error = validate(&with_schema, &mariadb).unwrap_err().to_string();
    assert!(error.contains("PostgreSQL"), "{error}");
    mariadb.services[1].engine = mugiurushi::database::DatabaseEngine::Postgres;
    let postgres = mariadb;
    validate(&with_schema, &postgres).unwrap();
    let sql = query(
        mugiurushi::database::DatabaseEngine::Postgres,
        &with_schema.database[0],
        "x",
    );
    assert!(sql.contains("FROM \"clinic\".\"patient\""), "{sql}");
    with_schema.database[0].schema = Some("clinic\".\"x".into());
    assert!(validate(&with_schema, &postgres).is_err());
    // 保存した形から読み直せる。
    with_schema.database[0].schema = Some("clinic".into());
    let saved = serde_json::to_string(&with_schema).unwrap();
    let read: BrowserSpec = serde_json::from_str(&saved).unwrap();
    assert_eq!(read.database[0].schema.as_deref(), Some("clinic"));
}
#[test]
fn response_wait_requires_a_click_and_registered_watch() {
    let mut value = serde_json::to_value(spec()).unwrap();
    value["actions"][0] = json!({"id":"send","action":"click","target":{"locator":{"by":"css","selector":"#send"}},"wait_response":"save"});
    let valid: BrowserSpec = serde_json::from_value(value.clone()).unwrap();
    validate(&valid, &containers()).unwrap();
    value["actions"][0]["wait_response"] = json!("unknown");
    let invalid: BrowserSpec = serde_json::from_value(value.clone()).unwrap();
    assert!(validate(&invalid, &containers()).is_err());
    value["actions"][0] = json!({"id":"show","action":"read_text","target":{"locator":{"by":"css","selector":"#message"}},"wait_response":"save"});
    let invalid: BrowserSpec = serde_json::from_value(value).unwrap();
    assert!(validate(&invalid, &containers()).is_err());
}

#[test]
fn absent_and_failed_reads_do_not_pass_negative_expectations() {
    let mut p = phase();
    for method in [
        CheckMethod::BrowserText,
        CheckMethod::BrowserResponse,
        CheckMethod::DatabaseRows,
    ] {
        assert_eq!(
            evaluate(
                &check(method, Expectation::NotContains, "missing"),
                Some(&p),
                "test"
            )
            .0,
            Observed::NotObserved
        );
    }
    p.database_reads.push(DatabaseRead {
        id: "row".into(),
        performed: true,
        exit_code: Some(1),
        text: "".into(),
        detail: "読取失敗".into(),
    });
    assert_eq!(
        evaluate(
            &check(CheckMethod::DatabaseRows, Expectation::NotContains, "row"),
            Some(&p),
            "test"
        )
        .0,
        Observed::NotObserved
    );
    p.database_reads[0].exit_code = Some(0);
    assert_eq!(
        evaluate(
            &check(CheckMethod::DatabaseRows, Expectation::NotContains, "row"),
            Some(&p),
            "test"
        )
        .0,
        Observed::Pass
    );
}

#[test]
fn database_relation_distinguishes_matches_mismatches_and_missing_observations() {
    let relation: TrialCheck = serde_json::from_value(json!({
        "id":"identity", "scope":"患者との対応", "method":"database_relation", "expect":"equal",
        "relation":{"left":"patient", "left_column":1, "right":"soap", "right_column":1}
    }))
    .unwrap();
    let mut p = phase();
    let read = |id: &str, text: &str| DatabaseRead {
        id: id.into(),
        performed: true,
        exit_code: Some(0),
        text: text.into(),
        detail: String::new(),
    };
    p.database_reads = vec![read("patient", "patient\t42\n"), read("soap", "soap\t42\n")];
    assert_eq!(evaluate(&relation, Some(&p), "").0, Observed::Pass);
    p.database_reads[1].text = "soap\t43\n".into();
    assert_eq!(evaluate(&relation, Some(&p), "").0, Observed::Fail);
    p.database_reads[1].text.clear();
    assert_eq!(evaluate(&relation, Some(&p), "").0, Observed::Fail);
    p.database_reads[1].exit_code = Some(1);
    assert_eq!(evaluate(&relation, Some(&p), "").0, Observed::NotObserved);
    p.database_reads[1].exit_code = Some(0);
    for text in ["soap\tNULL\n", "soap\t42\nsoap\t42\n", "soap\n"] {
        p.database_reads[1].text = text.into();
        assert_eq!(evaluate(&relation, Some(&p), "").0, Observed::NotObserved);
    }
}
#[test]
fn display_does_not_establish_database_success_and_mixed_http_responses_are_unknown() {
    let mut p = phase();
    assert_eq!(
        evaluate(
            &check(CheckMethod::BrowserText, Expectation::Contains, "message"),
            Some(&p),
            "完了"
        )
        .0,
        Observed::Pass
    );
    assert_eq!(
        evaluate(
            &check(CheckMethod::DatabaseRows, Expectation::Contains, "row"),
            Some(&p),
            "完了"
        )
        .0,
        Observed::NotObserved
    );
    let browser = p.browser.as_mut().unwrap();
    browser.responses = vec![
        ResponseRecord {
            payload_sha256: None,
            headers: None,
            watch: "save".into(),
            method: "POST".into(),
            path: "/save".into(),
            status: 200,
            body_match: Some(true),
            request_step: Some("message".into()),
        },
        ResponseRecord {
            payload_sha256: None,
            headers: None,
            watch: "save".into(),
            method: "POST".into(),
            path: "/save".into(),
            status: 500,
            body_match: None,
            request_step: Some("message".into()),
        },
    ];
    assert_eq!(
        evaluate(
            &check(CheckMethod::BrowserResponse, Expectation::Contains, "save"),
            Some(&p),
            "status=200"
        )
        .0,
        Observed::NotObserved
    );
    p.browser.as_mut().unwrap().responses.truncate(1);
    assert_eq!(
        evaluate(
            &check(CheckMethod::BrowserResponse, Expectation::Contains, "save"),
            Some(&p),
            "status=200"
        )
        .0,
        Observed::Pass
    );
    p.browser.as_mut().unwrap().response_limit = true;
    assert_eq!(
        evaluate(
            &check(CheckMethod::BrowserResponse, Expectation::Contains, "save"),
            Some(&p),
            "status=200"
        )
        .0,
        Observed::NotObserved
    );
    p.browser.as_mut().unwrap().response_limit = false;
    p.browser.as_mut().unwrap().responses[0].request_step = Some("username".into());
    assert_eq!(
        evaluate(
            &check(CheckMethod::BrowserResponse, Expectation::Contains, "save"),
            Some(&p),
            "status=200"
        )
        .0,
        Observed::NotObserved
    );
}

#[test]
fn validates_the_pinned_openemr_recipe_without_running_it() {
    let recipe: mugiurushi::execution::Recipe =
        serde_json::from_str(include_str!("../examples/openemr-browser/recipe.json")).unwrap();
    mugiurushi::execution::validate_recipe(&recipe).unwrap();
    assert_eq!(recipe.browser.as_ref().unwrap().database.len(), 2);
}

#[test]
fn validates_the_pinned_openmrs_recipe_without_running_it() {
    let recipe: mugiurushi::execution::Recipe = serde_json::from_str(include_str!(
        "../examples/openmrs-browser/register-recipe.json"
    ))
    .unwrap();
    mugiurushi::execution::validate_recipe(&recipe).unwrap();
    let services = &recipe.containers.as_ref().unwrap().services;
    assert_eq!(services.len(), 4);
    // 初回の起動でDBのスキーマを作るバックエンドは、5分では準備できない。
    let backend = services.iter().find(|s| s.name == "backend").unwrap();
    assert!(backend.ready_timeout_ms > 300_000);
    assert!(backend.ready_timeout_ms <= mugiurushi::container::MAX_READY_MS);
}

#[test]
fn relation_requires_existing_distinct_probes_and_columns() {
    let mut recipe: mugiurushi::execution::Recipe =
        serde_json::from_str(include_str!("../examples/openemr-browser/soap-recipe.json")).unwrap();
    mugiurushi::execution::validate_recipe(&recipe).unwrap();
    let check = recipe
        .trial
        .as_mut()
        .unwrap()
        .checks
        .iter_mut()
        .find(|c| c.method == CheckMethod::DatabaseRelation)
        .unwrap();
    check.relation.as_mut().unwrap().right_column = 100;
    assert!(mugiurushi::execution::validate_recipe(&recipe).is_err());
}

#[test]
fn relation_can_compare_recorded_before_and_after_values() {
    let mut check: TrialCheck = serde_json::from_value(json!({
        "id":"unchanged", "scope":"既存の記録", "method":"database_relation", "expect":"equal",
        "relation":{"left":"b", "left_column":0, "left_before":true, "right":"b", "right_column":0}
    }))
    .unwrap();
    let mut p = phase();
    let read = |text: &str| DatabaseRead {
        id: "b".into(),
        performed: true,
        exit_code: Some(0),
        text: text.into(),
        detail: String::new(),
    };
    p.database_reads_before.push(read("old\n"));
    p.database_reads.push(read("old\n"));
    assert_eq!(evaluate(&check, Some(&p), "").0, Observed::Pass);
    p.database_reads[0].text = "new\n".into();
    assert_eq!(evaluate(&check, Some(&p), "").0, Observed::Fail);
    check.expect = Expectation::NotEqual;
    assert_eq!(evaluate(&check, Some(&p), "").0, Observed::Pass);
    p.database_reads_before.clear();
    assert_eq!(evaluate(&check, Some(&p), "").0, Observed::NotObserved);
}

#[test]
fn dismissal_is_preparation_only_and_validates_both_targets() {
    let dismissal = json!({
        "id":"dismiss-notice", "action":"dismiss",
        "target":{"locator":{"by":"css","selector":"#notice button"}},
        "content":{"locator":{"by":"css","selector":"#notice"}}
    });
    let mut valid = spec();
    valid
        .preparation
        .push(serde_json::from_value(dismissal.clone()).unwrap());
    validate(&valid, &containers()).unwrap();

    let mut action_only = spec();
    action_only
        .actions
        .push(serde_json::from_value(dismissal.clone()).unwrap());
    assert!(validate(&action_only, &containers()).is_err());

    let mut invalid = dismissal;
    invalid["content"]["locator"]["selector"] = json!("");
    let mut bad = spec();
    bad.preparation
        .push(serde_json::from_value(invalid).unwrap());
    assert!(validate(&bad, &containers()).is_err());
}

#[test]
fn unknown_response_body_never_passes_negative_body_check_and_expressions_are_validated() {
    let mut p = phase();
    p.browser.as_mut().unwrap().responses.push(ResponseRecord {
        payload_sha256: None,
        headers: None,
        watch: "save".into(),
        method: "POST".into(),
        path: "/save".into(),
        status: 200,
        body_match: None,
        request_step: Some("message".into()),
    });
    assert_eq!(
        evaluate(
            &check(
                CheckMethod::BrowserResponse,
                Expectation::NotContains,
                "save"
            ),
            Some(&p),
            "body_match=true"
        )
        .0,
        Observed::NotObserved
    );
    let mut recipe: mugiurushi::execution::Recipe =
        serde_json::from_str(include_str!("../examples/openemr-browser/recipe.json")).unwrap();
    recipe.trial.as_mut().unwrap().checks[1].text = Some("false".into());
    assert!(mugiurushi::execution::validate_recipe(&recipe).is_err());
}

#[test]
fn missing_pixels_or_payload_hashes_do_not_satisfy_negative_expectations() {
    let mut p = phase();
    assert_eq!(
        evaluate(
            &check(
                CheckMethod::BrowserCanvas,
                Expectation::NotContains,
                "message"
            ),
            Some(&p),
            "variation=true"
        )
        .0,
        Observed::NotObserved
    );
    p.browser.as_mut().unwrap().steps[0].canvas=Some(serde_json::from_value(json!({"width":64,"height":64,"levels":1,"non_black_pixels":0,"rgba_sha256":"synthetic","variation":false})).unwrap());
    assert_eq!(
        evaluate(
            &check(CheckMethod::BrowserCanvas, Expectation::Contains, "message"),
            Some(&p),
            "variation=true"
        )
        .0,
        Observed::Fail
    );
    p.http_reads.push(serde_json::from_value(json!({"id":"direct","performed":true,"status":503,"content_type":"text/html","bytes":null,"sha256":null,"detail":"frame未読"})).unwrap());
    assert_eq!(
        evaluate(
            &check(CheckMethod::ServerHttp, Expectation::Contains, "direct"),
            Some(&p),
            "status=200"
        )
        .0,
        Observed::Fail
    );
    assert_eq!(
        evaluate(
            &check(CheckMethod::ServerHttp, Expectation::NotContains, "direct"),
            Some(&p),
            "sha256=missing"
        )
        .0,
        Observed::NotObserved
    );
    p.browser.as_mut().unwrap().responses.push(ResponseRecord {
        watch: "save".into(),
        method: "POST".into(),
        path: "/save".into(),
        status: 503,
        body_match: None,
        request_step: Some("message".into()),
        payload_sha256: None,
        headers: None,
    });
    assert_eq!(
        evaluate(
            &check(
                CheckMethod::BrowserResponse,
                Expectation::NotContains,
                "save"
            ),
            Some(&p),
            "sha256=missing"
        )
        .0,
        Observed::NotObserved
    );
}

#[test]
fn rejection_requires_the_declared_action_response_and_a_complete_record() {
    use mugiurushi::browser::{RejectionWatch, rejection_observed};
    let mut s = spec();
    s.rejection = Some(RejectionWatch {
        watch: "save".into(),
        status: 503,
    });
    let mut p = phase();
    let report = p.browser.as_mut().unwrap();
    assert!(!rejection_observed(&s, report));
    report.responses.push(ResponseRecord {
        watch: "save".into(),
        method: "POST".into(),
        path: "/save".into(),
        status: 503,
        body_match: None,
        request_step: Some("message".into()),
        payload_sha256: None,
        headers: None,
    });
    assert!(rejection_observed(&s, report));
    report.responses[0].request_step = Some("username".into());
    assert!(!rejection_observed(&s, report));
    report.responses[0].request_step = Some("message".into());
    report.response_limit = true;
    assert!(!rejection_observed(&s, report));
}

fn with_browser(edit: impl FnOnce(&mut serde_json::Value)) -> ContainerPhase {
    let mut value = serde_json::to_value(phase()).unwrap();
    edit(&mut value["browser"]);
    serde_json::from_value(value).unwrap()
}

#[test]
fn an_attribute_is_judged_only_when_the_element_was_read() {
    use CheckMethod::BrowserAttribute as Attribute;
    let observe = |p: &ContainerPhase, expect, text: &str| {
        evaluate(&check(Attribute, expect, "kind"), Some(p), text).0
    };
    let read = |attribute: serde_json::Value| {
        with_browser(|b| {
            b["steps"] = json!([{"id":"kind","phase":"action","completed":true,"text":null,
                "screenshot":null,"detail":"合成","attribute":attribute}]);
        })
    };
    let password = read(json!({"present":true,"value":"password"}));
    assert_eq!(
        observe(&password, Expectation::Equal, "password"),
        Observed::Pass
    );
    assert_eq!(
        observe(&password, Expectation::Equal, "pass"),
        Observed::Fail
    );
    assert_eq!(
        observe(&password, Expectation::Contains, "pass"),
        Observed::Pass
    );
    assert_eq!(
        observe(&password, Expectation::NotEqual, "text"),
        Observed::Pass
    );
    assert_eq!(
        observe(&password, Expectation::NotContains, "word"),
        Observed::Fail
    );

    // 要素はあるが属性がない場合は、観測した結果として扱う。
    let absent = read(json!({"present":false,"value":null}));
    assert_eq!(observe(&absent, Expectation::Equal, "off"), Observed::Fail);
    assert_eq!(
        observe(&absent, Expectation::Contains, "off"),
        Observed::Fail
    );
    assert_eq!(
        observe(&absent, Expectation::NotEqual, "on"),
        Observed::Pass
    );
    assert_eq!(
        observe(&absent, Expectation::NotContains, "on"),
        Observed::Pass
    );

    // 手順が未完了、準備の段階の手順、属性を読んでいない手順は、否定の条件も満たさない。
    let incomplete = with_browser(|b| {
        b["steps"] = json!([{"id":"kind","phase":"action","completed":false,"text":null,
            "screenshot":null,"detail":"合成"}]);
    });
    let preparation = with_browser(|b| {
        b["steps"] = json!([{"id":"kind","phase":"preparation","completed":true,"text":null,
            "screenshot":null,"detail":"合成","attribute":{"present":true,"value":"password"}}]);
    });
    let unread = with_browser(|b| {
        b["steps"] = json!([{"id":"kind","phase":"action","completed":true,"text":"x",
            "screenshot":null,"detail":"合成"}]);
    });
    for p in [&incomplete, &preparation, &unread] {
        assert_eq!(
            observe(p, Expectation::NotEqual, "on"),
            Observed::NotObserved
        );
        assert_eq!(
            observe(p, Expectation::Equal, "password"),
            Observed::NotObserved
        );
    }
    assert_eq!(
        evaluate(
            &check(Attribute, Expectation::Equal, "kind"),
            None,
            "password"
        )
        .0,
        Observed::NotObserved
    );
}

#[test]
fn a_header_is_judged_only_from_recorded_action_responses() {
    use CheckMethod::BrowserHeader as Header;
    let observe = |p: &ContainerPhase, expect, text: &str| {
        evaluate(&check(Header, expect, "save"), Some(p), text).0
    };
    let response = |headers: serde_json::Value| {
        json!({"watch":"save","method":"POST","path":"/save","status":200,"body_match":null,
            "request_step":"message","payload_sha256":null,"headers":headers})
    };
    let stored = with_browser(|b| {
        b["responses"] = json!([response(json!({"cache-control":"no-store, private"}))]);
    });
    assert_eq!(
        observe(&stored, Expectation::Contains, "Cache-Control: no-store"),
        Observed::Pass
    );
    assert_eq!(
        observe(&stored, Expectation::Contains, "cache-control: public"),
        Observed::Fail
    );
    assert_eq!(
        observe(&stored, Expectation::NotContains, "cache-control: public"),
        Observed::Pass
    );
    // 値を書かない条件は、ヘッダーの有無を確かめる。
    assert_eq!(
        observe(&stored, Expectation::Contains, "cache-control:"),
        Observed::Pass
    );
    assert_eq!(
        observe(&stored, Expectation::Contains, "x-frame-options:"),
        Observed::Fail
    );
    assert_eq!(
        observe(&stored, Expectation::NotContains, "x-frame-options:"),
        Observed::Pass
    );

    // ヘッダーを記録していない変更前の記録、応答のない記録、上限に達した記録は判定しない。
    let old = with_browser(|b| {
        let mut r = response(json!({}));
        r.as_object_mut().unwrap().remove("headers");
        b["responses"] = json!([r]);
    });
    let none = phase();
    let limited = with_browser(|b| {
        b["responses"] = json!([response(json!({"cache-control":"no-store"}))]);
        b["response_limit"] = json!(true);
    });
    let preparation = with_browser(|b| {
        let mut r = response(json!({"cache-control":"no-store"}));
        r["request_step"] = json!("username");
        b["responses"] = json!([r]);
    });
    for p in [&old, &none, &limited, &preparation] {
        assert_eq!(
            observe(p, Expectation::NotContains, "x-frame-options:"),
            Observed::NotObserved
        );
        assert_eq!(
            observe(p, Expectation::Contains, "cache-control: no-store"),
            Observed::NotObserved
        );
    }
    // 同じ観測先の応答で結果が分かれる場合と、記録の対象にしていない名前は判定しない。
    let mixed = with_browser(|b| {
        b["responses"] = json!([
            response(json!({"cache-control":"no-store"})),
            response(json!({"cache-control":"public"}))
        ]);
    });
    assert_eq!(
        observe(&mixed, Expectation::Contains, "cache-control: no-store"),
        Observed::NotObserved
    );
    assert_eq!(
        observe(&stored, Expectation::NotContains, "authorization: x"),
        Observed::NotObserved
    );
}

#[test]
fn attribute_and_header_checks_must_name_a_matching_step_or_watch() {
    let recipe = |actions: serde_json::Value, check: serde_json::Value| {
        let mut browser = serde_json::to_value(spec()).unwrap();
        browser["actions"] = actions;
        let value = json!({
            "schema_version":1,"check_id":"c",
            "source":{"repository":"example/app","commit":"a".repeat(40),"path":"app.py","line":1,"end_line":1},
            "runtime":"browser","entrypoint":"/","args":[],"input":{},
            "criterion":{"assertion":"eel.trial","subject":"c"},"timeout_ms":5000,
            "trial":{"schema_version":1,"fault":"normal","storage":null,"readback":null,"checks":[check]},
            "containers":serde_json::to_value(containers()).unwrap(),
            "browser":browser
        });
        serde_json::from_value::<mugiurushi::execution::Recipe>(value)
            .map_err(std::io::Error::other)
            .and_then(|r| mugiurushi::execution::validate_recipe(&r))
    };
    let read = json!([{"id":"kind","action":"read_attribute","name":"type",
        "target":{"locator":{"by":"label","text":"暗証"}}}]);
    let attribute = |expect: &str, target: &str, text: serde_json::Value| json!({"id":"k","scope":"入力欄","method":"browser_attribute","expect":expect,"target":target,"text":text});
    recipe(read.clone(), attribute("equal", "kind", json!("password"))).unwrap();
    recipe(
        read.clone(),
        attribute("not_contains", "kind", json!("text")),
    )
    .unwrap();
    // 読む手順がない、文がない、属性の名前が不正。
    assert!(recipe(read.clone(), attribute("equal", "other", json!("password"))).is_err());
    assert!(recipe(read.clone(), attribute("equal", "kind", json!(null))).is_err());
    assert!(recipe(read.clone(), attribute("zero", "kind", json!("password"))).is_err());
    let bad_name = json!([{"id":"kind","action":"read_attribute","name":"on click",
        "target":{"locator":{"by":"label","text":"暗証"}}}]);
    assert!(recipe(bad_name, attribute("equal", "kind", json!("password"))).is_err());

    let header = |target: &str, text: &str, expect: &str| json!({"id":"h","scope":"応答","method":"browser_header","expect":expect,"target":target,"text":text});
    recipe(
        read.clone(),
        header("save", "Cache-Control: no-store", "contains"),
    )
    .unwrap();
    recipe(
        read.clone(),
        header("save", "x-frame-options:", "not_contains"),
    )
    .unwrap();
    // 宣言していない観測先、記録の対象にしていない名前、名前のない文、等値の指定。
    assert!(
        recipe(
            read.clone(),
            header("other", "cache-control: no-store", "contains")
        )
        .is_err()
    );
    assert!(recipe(read.clone(), header("save", "authorization: x", "contains")).is_err());
    assert!(recipe(read.clone(), header("save", "no-store", "contains")).is_err());
    assert!(recipe(read, header("save", "cache-control: no-store", "equal")).is_err());
}

#[test]
fn records_saved_before_screens_and_exchanges_still_load_and_keep_their_shape() {
    // 変更前の形の記録。新しい項目を持たない。
    let before = json!({"schema_version":1,"run_id":"test","version":"151.0.7922.34","prepared":true,
        "completed":true,"steps":[{"id":"message","phase":"action","completed":true,"text":"完了",
        "screenshot":null,"detail":"合成"}],
        "responses":[{"watch":"save","method":"POST","path":"/save","status":200,"body_match":true,
        "request_step":"message","payload_sha256":null}],
        "blocked_requests":0,"response_limit":false,"detail":"合成"});
    let report: mugiurushi::browser::BrowserReport = serde_json::from_value(before).unwrap();
    assert!(report.screens.is_empty() && report.screens_note.is_empty());
    assert!(report.exchanges.is_empty() && !report.exchange_limit);
    assert!(report.responses[0].headers.is_none() && report.steps[0].attribute.is_none());
    let saved = serde_json::to_value(&report).unwrap();
    for key in ["screens", "screens_note", "exchanges", "exchange_limit"] {
        assert!(saved.get(key).is_none(), "{key}");
    }
    assert!(saved["responses"][0].get("headers").is_none());
    assert!(saved["steps"][0].get("attribute").is_none());
    // 新しい操作を使わないレシピの保存形も変わらない。
    let browser = serde_json::to_string(&spec()).unwrap();
    assert!(!browser.contains("read_attribute"));
}

#[test]
fn the_relay_cut_fault_and_the_cut_point_must_be_declared_together() {
    let recipe = |fault: &str, relay: serde_json::Value| {
        let mut containers = serde_json::to_value(containers()).unwrap();
        containers["relay"] = relay;
        let value = json!({
            "schema_version":1,"check_id":"c",
            "source":{"repository":"example/app","commit":"a".repeat(40),"path":"app.py","line":1,"end_line":1},
            "runtime":"browser","entrypoint":"/","args":[],"input":{},
            "criterion":{"assertion":"eel.trial","subject":"c"},"timeout_ms":5000,
            "trial":{"schema_version":1,"fault":fault,"storage":null,"readback":null,"checks":[
                {"id":"shown","scope":"表示","method":"browser_text","expect":"contains","target":"message","text":"完了"}]},
            "containers":containers,
            "browser":serde_json::to_value(spec()).unwrap()
        });
        serde_json::from_value::<mugiurushi::execution::Recipe>(value)
            .map_err(std::io::Error::other)
            .and_then(|r| mugiurushi::execution::validate_recipe(&r))
    };
    let record_only = json!({"service":"db","protocol":"mysql"});
    let with_cut = json!({"service":"db","protocol":"mysql",
        "cut":{"shape":"0123456789abcdef","occurrence":1,"phase":"before_forward"}});
    // 記録だけの中継は、どの故障とも組み合わせられる。
    recipe("normal", record_only.clone()).unwrap();
    recipe("database_stopped", record_only.clone()).unwrap();
    recipe("relay_cut", with_cut.clone()).unwrap();
    // 切る時点のない切断の故障と、切断の故障ではない試験での切る時点は、どちらも拒否する。
    assert!(recipe("relay_cut", record_only).is_err());
    assert!(recipe("normal", with_cut.clone()).is_err());
    assert!(recipe("database_stopped", with_cut).is_err());
}
