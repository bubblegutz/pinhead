use std::collections::HashMap;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use pinhead::fsop::FsOperation;
use pinhead::handler::{HandlerRequest, HandlerResponse};
use pinhead::router;

/// Wrapper: build a RouteMeta with a wildcard ("*") handler.
fn wildcard_handler(name: &str) -> pinhead::router::RouteMeta {
    pinhead::router::RouteMeta {
        handlers: HashMap::from([("*".to_string(), name.to_string())]),
    }
}

/// Helper: build a router, a closure handler, send a request, and assert
/// the response comes back with the expected content.
#[tokio::test]
async fn test_single_route_lookup() {
    let mut rb = router::new();
    rb.register("/hello", wildcard_handler("hello_handler"))
        .unwrap();

    let (handler_tx, mut handler_rx) = mpsc::channel::<HandlerRequest>(16);
    let (frontend_tx, _router_h) = rb.build(handler_tx);

    tokio::spawn(async move {
        while let Some(req) = handler_rx.recv().await {
            let body = format!("handler={} params={:?}", req.handler_name, req.params);
            let _ = req
                .reply
                .send(Ok(HandlerResponse { data: Bytes::from(body) }));
        }
    });

    let (reply_tx, reply_rx) = oneshot::channel();
    frontend_tx
        .send(pinhead::router::Request {
            op: FsOperation::Lookup,
            path: "/hello".into(),
            data: Bytes::new(),
            reply: reply_tx,
        })
        .await
        .unwrap();

    let resp = reply_rx.await.unwrap().unwrap();
    let text = String::from_utf8_lossy(&resp.data);
    assert!(text.contains("hello_handler"), "response contains handler name");
}

/// Test that route params are captured correctly.
#[tokio::test]
async fn test_route_params() {
    let mut rb = router::new();
    rb.register(
        "/users/{id}/profile",
        wildcard_handler("profile_handler"),
    )
    .unwrap();

    let (handler_tx, mut handler_rx) = mpsc::channel::<HandlerRequest>(16);
    let (frontend_tx, _router_h) = rb.build(handler_tx);

    tokio::spawn(async move {
        while let Some(req) = handler_rx.recv().await {
            let body = format!("params={:?}", req.params);
            let _ = req
                .reply
                .send(Ok(HandlerResponse { data: Bytes::from(body) }));
        }
    });

    let (reply_tx, reply_rx) = oneshot::channel();
    frontend_tx
        .send(pinhead::router::Request {
            op: FsOperation::GetAttr,
            path: "/users/42/profile".into(),
            data: Bytes::new(),
            reply: reply_tx,
        })
        .await
        .unwrap();

    let resp = reply_rx.await.unwrap().unwrap();
    let text = String::from_utf8_lossy(&resp.data);
    assert!(
        text.contains("\"id\": \"42\""),
        "response contains captured param: {text:?}"
    );
}

/// Test that a wildcard catch-all route works.
#[tokio::test]
async fn test_wildcard_route() {
    let mut rb = router::new();
    rb.register("/static/{*path}", wildcard_handler("static_handler"))
        .unwrap();

    let (handler_tx, mut handler_rx) = mpsc::channel::<HandlerRequest>(16);
    let (frontend_tx, _router_h) = rb.build(handler_tx);

    tokio::spawn(async move {
        while let Some(req) = handler_rx.recv().await {
            let body = format!("params={:?}", req.params);
            let _ = req
                .reply
                .send(Ok(HandlerResponse { data: Bytes::from(body) }));
        }
    });

    let (reply_tx, reply_rx) = oneshot::channel();
    frontend_tx
        .send(pinhead::router::Request {
            op: FsOperation::Read,
            path: "/static/css/style.css".into(),
            data: Bytes::new(),
            reply: reply_tx,
        })
        .await
        .unwrap();

    let resp = reply_rx.await.unwrap().unwrap();
    let text = String::from_utf8_lossy(&resp.data);
    assert!(
        text.contains("\"path\": \"css/style.css\""),
        "wildcard captures full remainder: {text:?}"
    );
}

/// Test that unmatched paths return an error.
#[tokio::test]
async fn test_unmatched_path() {
    let mut rb = router::new();
    rb.register("/known", wildcard_handler("handler")).unwrap();

    let (handler_tx, _handler_rx) = mpsc::channel::<HandlerRequest>(16);
    let (frontend_tx, _router_h) = rb.build(handler_tx);

    let (reply_tx, reply_rx) = oneshot::channel();
    frontend_tx
        .send(pinhead::router::Request {
            op: FsOperation::Lookup,
            path: "/unknown".into(),
            data: Bytes::new(),
            reply: reply_tx,
        })
        .await
        .unwrap();

    let result = reply_rx.await.unwrap();
    assert!(result.is_err(), "unmatched path should return error");
}

/// Test that multiple concurrent requests are all handled.
#[tokio::test]
async fn test_concurrent_requests() {
    let mut rb = router::new();
    rb.register("/a", wildcard_handler("a")).unwrap();
    rb.register("/b", wildcard_handler("b")).unwrap();

    let (handler_tx, mut handler_rx) = mpsc::channel::<HandlerRequest>(64);
    let (frontend_tx, _router_h) = rb.build(handler_tx);

    tokio::spawn(async move {
        while let Some(req) = handler_rx.recv().await {
            let body = format!("OK {}", req.handler_name);
            let _ = req
                .reply
                .send(Ok(HandlerResponse { data: Bytes::from(body) }));
        }
    });

    let tx = frontend_tx.clone();
    let h1 = tokio::spawn(async move {
        let (rt, rr) = oneshot::channel();
        tx.send(pinhead::router::Request {
            op: FsOperation::Read,
            path: "/a".into(),
            data: Bytes::new(),
            reply: rt,
        })
        .await
        .unwrap();
        rr.await.unwrap().unwrap()
    });
    let h2 = tokio::spawn(async move {
        let (rt, rr) = oneshot::channel();
        frontend_tx
            .send(pinhead::router::Request {
                op: FsOperation::Read,
                path: "/b".into(),
                data: Bytes::new(),
                reply: rt,
            })
            .await
            .unwrap();
        rr.await.unwrap().unwrap()
    });

    let (r1, r2) = tokio::join!(h1, h2);
    assert_eq!(String::from_utf8_lossy(&r1.unwrap().data), "OK a");
    assert_eq!(String::from_utf8_lossy(&r2.unwrap().data), "OK b");
}
