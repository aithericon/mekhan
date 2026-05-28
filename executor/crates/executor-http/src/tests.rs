use std::collections::HashMap;

use super::*;

#[test]
fn http_config_minimal_deserialize() {
    let json = serde_json::json!({
        "url": "https://example.com/api"
    });
    let config: HttpConfig = serde_json::from_value(json).unwrap();
    assert_eq!(config.url, "https://example.com/api");
    assert_eq!(config.method, HttpMethod::GET);
    assert!(config.follow_redirects);
    assert_eq!(config.max_response_bytes, 1_048_576);
    assert_eq!(config.response_mode, ResponseMode::Auto);
    assert!(config.headers.is_empty());
    assert!(config.query.is_empty());
    assert!(config.body.is_none());
    assert!(config.body_from_input.is_none());
    assert!(config.auth.is_none());
    assert!(config.expected_status_codes.is_empty());
    assert!(!config.danger_accept_invalid_certs);
    assert!(config.output_mapping.is_empty());
}

#[test]
fn http_config_output_mapping_deserialize() {
    let json = serde_json::json!({
        "url": "https://example.com/api",
        "output_mapping": {
            "user_id": "body.data.id",
            "req_id": "headers.x-request-id"
        }
    });
    let config: HttpConfig = serde_json::from_value(json).unwrap();
    assert_eq!(config.output_mapping.len(), 2);
    assert_eq!(config.output_mapping["user_id"], "body.data.id");
    assert_eq!(config.output_mapping["req_id"], "headers.x-request-id");
}

#[test]
fn http_config_full_roundtrip() {
    let config = HttpConfig {
        method: HttpMethod::POST,
        url: "https://api.example.com/{{path}}".into(),
        headers: HashMap::from([("X-Custom".into(), "value".into())]),
        query: HashMap::from([("page".into(), "1".into())]),
        body: Some(serde_json::json!({"key": "value"})),
        body_from_input: None,
        auth_resource: None,
        auth: Some(AuthConfig::Bearer {
            token: None,
            token_env: Some("API_TOKEN".into()),
        }),
        timeout_secs: Some(30),
        follow_redirects: false,
        expected_status_codes: vec![200, 201],
        response_mode: ResponseMode::Json,
        max_response_bytes: 2_000_000,
        danger_accept_invalid_certs: true,
        output_mapping: HashMap::new(),
    };

    let json = serde_json::to_value(&config).unwrap();
    let roundtripped: HttpConfig = serde_json::from_value(json).unwrap();

    assert_eq!(roundtripped.method, HttpMethod::POST);
    assert_eq!(roundtripped.url, "https://api.example.com/{{path}}");
    assert_eq!(roundtripped.timeout_secs, Some(30));
    assert!(!roundtripped.follow_redirects);
    assert_eq!(roundtripped.expected_status_codes, vec![200, 201]);
    assert_eq!(roundtripped.response_mode, ResponseMode::Json);
    assert!(roundtripped.danger_accept_invalid_certs);
}

#[test]
fn http_config_from_spec() {
    let spec = ExecutionSpec {
        backend: "http".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({
            "method": "PUT",
            "url": "https://example.com",
        }),
            config_ref: None,
    };
    let config = HttpConfig::from_spec(&spec).unwrap();
    assert_eq!(config.method, HttpMethod::PUT);
    assert_eq!(config.url, "https://example.com");
}

#[test]
fn http_config_from_spec_invalid() {
    let spec = ExecutionSpec {
        backend: "http".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::json!({
            "method": "INVALID_METHOD",
            "url": "https://example.com",
        }),
            config_ref: None,
    };
    assert!(HttpConfig::from_spec(&spec).is_err());
}

#[test]
fn http_config_into_spec() {
    let config = HttpConfig {
        method: HttpMethod::GET,
        url: "https://example.com".into(),
        headers: HashMap::new(),
        query: HashMap::new(),
        body: None,
        body_from_input: None,
        auth_resource: None,
        auth: None,
        timeout_secs: None,
        follow_redirects: true,
        expected_status_codes: vec![],
        response_mode: ResponseMode::Auto,
        max_response_bytes: 1_048_576,
        danger_accept_invalid_certs: false,
        output_mapping: HashMap::new(),
    };
    let spec = config.into_spec();
    assert_eq!(spec.backend, "http");
    assert!(spec.inputs.is_empty());
    assert!(spec.outputs.is_empty());
}

#[test]
fn validate_empty_url() {
    let config = HttpConfig {
        method: HttpMethod::GET,
        url: "".into(),
        headers: HashMap::new(),
        query: HashMap::new(),
        body: None,
        body_from_input: None,
        auth_resource: None,
        auth: None,
        timeout_secs: None,
        follow_redirects: true,
        expected_status_codes: vec![],
        response_mode: ResponseMode::Auto,
        max_response_bytes: 1_048_576,
        danger_accept_invalid_certs: false,
        output_mapping: HashMap::new(),
    };
    assert!(config.validate().is_err());
}

#[test]
fn validate_body_and_body_from_input_conflict() {
    let config = HttpConfig {
        method: HttpMethod::POST,
        url: "https://example.com".into(),
        headers: HashMap::new(),
        query: HashMap::new(),
        body: Some(serde_json::json!("data")),
        body_from_input: Some("input.json".into()),
        auth: None,
        auth_resource: None,
        timeout_secs: None,
        follow_redirects: true,
        expected_status_codes: vec![],
        response_mode: ResponseMode::Auto,
        max_response_bytes: 1_048_576,
        danger_accept_invalid_certs: false,
        output_mapping: HashMap::new(),
    };
    assert!(config.validate().is_err());
}

#[test]
fn auth_bearer_serde() {
    let auth = AuthConfig::Bearer {
        token: Some("secret".into()),
        token_env: None,
    };
    let json = serde_json::to_value(&auth).unwrap();
    assert_eq!(json["type"], "bearer");
    assert_eq!(json["token"], "secret");

    let roundtripped: AuthConfig = serde_json::from_value(json).unwrap();
    match roundtripped {
        AuthConfig::Bearer { token, token_env } => {
            assert_eq!(token.unwrap(), "secret");
            assert!(token_env.is_none());
        }
        _ => panic!("expected Bearer"),
    }
}

#[test]
fn auth_basic_serde() {
    let auth = AuthConfig::Basic {
        username: "user".into(),
        password: None,
        password_env: Some("PASS_VAR".into()),
    };
    let json = serde_json::to_value(&auth).unwrap();
    assert_eq!(json["type"], "basic");
    assert_eq!(json["username"], "user");
    assert_eq!(json["password_env"], "PASS_VAR");
}

#[test]
fn auth_header_serde() {
    let auth = AuthConfig::Header {
        name: "X-API-Key".into(),
        value: Some("key123".into()),
        value_env: None,
    };
    let json = serde_json::to_value(&auth).unwrap();
    assert_eq!(json["type"], "header");
    assert_eq!(json["name"], "X-API-Key");
    assert_eq!(json["value"], "key123");
}

#[test]
fn resolve_auth_bearer_from_env() {
    let mut config = HttpConfig {
        method: HttpMethod::GET,
        url: "https://example.com".into(),
        headers: HashMap::new(),
        query: HashMap::new(),
        body: None,
        body_from_input: None,
        auth_resource: None,
        auth: Some(AuthConfig::Bearer {
            token: None,
            token_env: Some("MY_TOKEN".into()),
        }),
        timeout_secs: None,
        follow_redirects: true,
        expected_status_codes: vec![],
        response_mode: ResponseMode::Auto,
        max_response_bytes: 1_048_576,
        danger_accept_invalid_certs: false,
        output_mapping: HashMap::new(),
    };

    let env = HashMap::from([("MY_TOKEN".into(), "resolved_token".into())]);
    config.resolve_auth(&env).unwrap();

    match &config.auth {
        Some(AuthConfig::Bearer { token, .. }) => {
            assert_eq!(token.as_deref(), Some("resolved_token"));
        }
        _ => panic!("expected Bearer auth"),
    }
}

#[test]
fn resolve_auth_missing_env_var() {
    let mut config = HttpConfig {
        method: HttpMethod::GET,
        url: "https://example.com".into(),
        headers: HashMap::new(),
        query: HashMap::new(),
        body: None,
        body_from_input: None,
        auth_resource: None,
        auth: Some(AuthConfig::Bearer {
            token: None,
            token_env: Some("MISSING_VAR".into()),
        }),
        timeout_secs: None,
        follow_redirects: true,
        expected_status_codes: vec![],
        response_mode: ResponseMode::Auto,
        max_response_bytes: 1_048_576,
        danger_accept_invalid_certs: false,
        output_mapping: HashMap::new(),
    };

    let env = HashMap::new();
    assert!(config.resolve_auth(&env).is_err());
}

#[test]
fn supports_only_http_backend() {
    let backend = HttpBackend::new();
    let http_spec = ExecutionSpec {
        backend: "http".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::Value::Object(Default::default()),
        config_ref: None,
    };
    let process_spec = ExecutionSpec {
        backend: "process".into(),
        inputs: vec![],
        outputs: vec![],
        config: serde_json::Value::Object(Default::default()),
        config_ref: None,
    };
    assert!(backend.supports(&http_spec));
    assert!(!backend.supports(&process_spec));
}

#[test]
fn backend_name() {
    let backend = HttpBackend::new();
    assert_eq!(backend.name(), "http");
}

#[test]
fn method_serde() {
    assert_eq!(
        serde_json::to_value(HttpMethod::POST).unwrap(),
        serde_json::json!("POST")
    );
    assert_eq!(
        serde_json::from_value::<HttpMethod>(serde_json::json!("DELETE")).unwrap(),
        HttpMethod::DELETE
    );
}

#[test]
fn response_mode_serde() {
    assert_eq!(
        serde_json::to_value(ResponseMode::Json).unwrap(),
        serde_json::json!("json")
    );
    assert_eq!(
        serde_json::from_value::<ResponseMode>(serde_json::json!("text")).unwrap(),
        ResponseMode::Text
    );
}
