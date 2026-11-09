use axum::{
    body::Body,
    http::{Request, StatusCode},
    Router,
};
use hghub_web::{make_app, Config};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

fn app() -> Router {
    make_app(Config {
        allowed_hosts: vec!["localhost:8093".into()],
        dist: "/nonexistent-dist".into(),
    })
}

fn invoke(command: &str, args: Value) -> Request<Body> {
    Request::post("/api/invoke")
        .header("Host", "localhost:8093")
        .header("Origin", "http://localhost:8093")
        .header("Content-Type", "application/json")
        .header("X-HGhub-Web", "1")
        .body(Body::from(
            json!({"command": command, "args": args}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn unknown_host_is_rejected() {
    let request = Request::get("/healthz")
        .header("Host", "rebind.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn foreign_origin_and_simple_cross_site_posts_are_rejected() {
    for header in ["Origin", "X-HGhub-Web"] {
        let mut request = invoke("app_boot", json!({}));
        request.headers_mut().remove(header);
        if header == "Origin" {
            request
                .headers_mut()
                .insert("Origin", "https://other.example".parse().unwrap());
        }
        assert_eq!(
            app().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn boot_issues_private_cookie_without_sharing_upstream_login() {
    let service = app();
    let response = service
        .clone()
        .oneshot(invoke("app_boot", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let first = cookie.split(';').next().unwrap().to_string();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["data"]["logged_in"], false);
    let second = service
        .oneshot(invoke("app_boot", json!({})))
        .await
        .unwrap();
    assert_ne!(
        first,
        second.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
    );
}

#[tokio::test]
async fn server_security_cannot_be_disabled_from_the_browser() {
    let response = app()
        .oneshot(invoke("security_set", json!({"strict": false})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_pagination_is_rejected_before_upstream_request() {
    let response = app()
        .oneshot(invoke("api_feed", json!({"page": 0, "size": 10000})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn local_network_media_targets_are_rejected() {
    let request = Request::get("/media/seg?u=http%3A%2F%2F127.0.0.1%3A8080%2F")
        .header("Host", "localhost:8093")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn unknown_commands_are_not_an_arbitrary_rpc_gateway() {
    let response = app()
        .oneshot(invoke("plugin:app|exit", json!({})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversized_requests_are_rejected() {
    let response = app()
        .oneshot(invoke("api_search", json!({"q": "x".repeat(20000)})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
