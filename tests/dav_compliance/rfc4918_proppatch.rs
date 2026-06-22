//! RFC 4918 §9.2 PROPPATCH compliance — dead property storage and retrieval.

use reqwest::Method;

use super::harness::{get_server, unique_name};

fn propfind() -> Method {
    Method::from_bytes(b"PROPFIND").unwrap()
}

fn proppatch() -> Method {
    Method::from_bytes(b"PROPPATCH").unwrap()
}

/// PROPPATCH set a custom property → 207 with 200 propstat.
#[tokio::test]
async fn proppatch_set_returns_207() {
    let srv = get_server();
    let path = format!("/webdav/{}", unique_name("pp_set"));
    let (k, v) = srv.auth();

    srv.client()
        .put(srv.url(&path))
        .header(k, v.clone())
        .body("x")
        .send()
        .await
        .unwrap();

    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set>
    <D:prop>
      <Z:author>Alice</Z:author>
    </D:prop>
  </D:set>
</D:propertyupdate>"#;

    let res = srv
        .client()
        .request(proppatch(), srv.url(&path))
        .header(k, v)
        .header("Content-Type", "application/xml")
        .body(xml)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 207, "PROPPATCH must return 207");
    let body = res.text().await.unwrap();
    assert!(
        body.contains("200") || body.contains("HTTP/1.1 200"),
        "PROPPATCH 207 must contain 200 propstat; body: {body}"
    );
}

/// PROPPATCH set → PROPFIND retrieves the stored value.
#[tokio::test]
async fn proppatch_set_property_visible_in_propfind() {
    let srv = get_server();
    let path = format!("/webdav/{}", unique_name("pp_roundtrip"));
    let (k, v) = srv.auth();

    srv.client()
        .put(srv.url(&path))
        .header(k, v.clone())
        .body("data")
        .send()
        .await
        .unwrap();

    // Set dead property
    let set_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set>
    <D:prop>
      <Z:color>blue</Z:color>
    </D:prop>
  </D:set>
</D:propertyupdate>"#;

    let pp_res = srv
        .client()
        .request(proppatch(), srv.url(&path))
        .header(k, v.clone())
        .header("Content-Type", "application/xml")
        .body(set_xml)
        .send()
        .await
        .unwrap();
    assert_eq!(pp_res.status(), 207, "PROPPATCH set must return 207");

    // Retrieve via PROPFIND allprop
    let pf_res = srv
        .client()
        .request(propfind(), srv.url(&path))
        .header(k, v)
        .header("Depth", "0")
        .send()
        .await
        .unwrap();
    assert_eq!(pf_res.status(), 207);
    let body = pf_res.text().await.unwrap();
    assert!(
        body.contains("color") || body.contains("blue"),
        "PROPFIND allprop must include dead property set by PROPPATCH; body: {body}"
    );
}

/// PROPPATCH remove → property absent from subsequent PROPFIND.
#[tokio::test]
async fn proppatch_remove_property_not_in_propfind() {
    let srv = get_server();
    let path = format!("/webdav/{}", unique_name("pp_remove"));
    let (k, v) = srv.auth();

    srv.client()
        .put(srv.url(&path))
        .header(k, v.clone())
        .body("data")
        .send()
        .await
        .unwrap();

    // First set
    let set_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set><D:prop><Z:tag>removeme</Z:tag></D:prop></D:set>
</D:propertyupdate>"#;
    srv.client()
        .request(proppatch(), srv.url(&path))
        .header(k, v.clone())
        .header("Content-Type", "application/xml")
        .body(set_xml)
        .send()
        .await
        .unwrap();

    // Then remove
    let remove_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:remove><D:prop><Z:tag/></D:prop></D:remove>
</D:propertyupdate>"#;
    let rem_res = srv
        .client()
        .request(proppatch(), srv.url(&path))
        .header(k, v.clone())
        .header("Content-Type", "application/xml")
        .body(remove_xml)
        .send()
        .await
        .unwrap();
    assert_eq!(rem_res.status(), 207, "PROPPATCH remove must return 207");

    // Verify gone — request the specific prop, expect 404 propstat
    let pf_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:prop><Z:tag/></D:prop>
</D:propfind>"#;
    let pf_res = srv
        .client()
        .request(propfind(), srv.url(&path))
        .header(k, v)
        .header("Depth", "0")
        .header("Content-Type", "application/xml")
        .body(pf_xml)
        .send()
        .await
        .unwrap();
    assert_eq!(pf_res.status(), 207);
    let body = pf_res.text().await.unwrap();
    assert!(
        body.contains("404"),
        "Removed dead property must appear in 404 propstat; body: {body}"
    );
}

/// PROPPATCH set + remove in same request → both applied atomically.
#[tokio::test]
async fn proppatch_set_and_remove_in_same_request() {
    let srv = get_server();
    let path = format!("/webdav/{}", unique_name("pp_setrem"));
    let (k, v) = srv.auth();

    srv.client()
        .put(srv.url(&path))
        .header(k, v.clone())
        .body("x")
        .send()
        .await
        .unwrap();

    // Pre-seed a property to remove
    let seed_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set><D:prop><Z:old>gone</Z:old></D:prop></D:set>
</D:propertyupdate>"#;
    srv.client()
        .request(proppatch(), srv.url(&path))
        .header(k, v.clone())
        .header("Content-Type", "application/xml")
        .body(seed_xml)
        .send()
        .await
        .unwrap();

    // Set new + remove old in one request
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set><D:prop><Z:new>here</Z:new></D:prop></D:set>
  <D:remove><D:prop><Z:old/></D:prop></D:remove>
</D:propertyupdate>"#;
    let res = srv
        .client()
        .request(proppatch(), srv.url(&path))
        .header(k, v)
        .header("Content-Type", "application/xml")
        .body(xml)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 207, "combined set+remove must return 207");
    let body = res.text().await.unwrap();
    // Both ops should succeed
    assert!(
        !body.contains("409") && !body.contains("403"),
        "combined PROPPATCH must not fail; body: {body}"
    );
}

/// PROPPATCH on non-existent resource → 404.
#[tokio::test]
async fn proppatch_nonexistent_resource_returns_404() {
    let srv = get_server();
    let path = format!("/webdav/{}", unique_name("pp_ghost"));
    let (k, v) = srv.auth();

    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set><D:prop><Z:x>y</Z:x></D:prop></D:set>
</D:propertyupdate>"#;

    let res = srv
        .client()
        .request(proppatch(), srv.url(&path))
        .header(k, v)
        .header("Content-Type", "application/xml")
        .body(xml)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        404,
        "PROPPATCH on non-existent resource must return 404"
    );
}

/// PROPPATCH on collection (folder) → 207.
#[tokio::test]
async fn proppatch_on_collection_returns_207() {
    let srv = get_server();
    let col = format!("/webdav/{}", unique_name("pp_col"));
    let (k, v) = srv.auth();

    srv.client()
        .request(Method::from_bytes(b"MKCOL").unwrap(), srv.url(&col))
        .header(k, v.clone())
        .send()
        .await
        .unwrap();

    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set><D:prop><Z:desc>my folder</Z:desc></D:prop></D:set>
</D:propertyupdate>"#;

    let res = srv
        .client()
        .request(proppatch(), srv.url(&col))
        .header(k, v)
        .header("Content-Type", "application/xml")
        .body(xml)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 207, "PROPPATCH on collection must return 207");
}

/// PROPFIND specific dead property returns value in 200 propstat (not 404).
#[tokio::test]
async fn propfind_specific_dead_property_returns_200_propstat() {
    let srv = get_server();
    let path = format!("/webdav/{}", unique_name("pp_specific"));
    let (k, v) = srv.auth();

    srv.client()
        .put(srv.url(&path))
        .header(k, v.clone())
        .body("x")
        .send()
        .await
        .unwrap();

    // Set
    let set_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:set><D:prop><Z:rating>5</Z:rating></D:prop></D:set>
</D:propertyupdate>"#;
    srv.client()
        .request(proppatch(), srv.url(&path))
        .header(k, v.clone())
        .header("Content-Type", "application/xml")
        .body(set_xml)
        .send()
        .await
        .unwrap();

    // PROPFIND for that exact property
    let pf_xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:Z="http://example.com/ns/">
  <D:prop><Z:rating/></D:prop>
</D:propfind>"#;
    let pf_res = srv
        .client()
        .request(propfind(), srv.url(&path))
        .header(k, v)
        .header("Depth", "0")
        .header("Content-Type", "application/xml")
        .body(pf_xml)
        .send()
        .await
        .unwrap();
    assert_eq!(pf_res.status(), 207);
    let body = pf_res.text().await.unwrap();
    assert!(
        !body.contains("404"),
        "Known dead property must not be in 404 propstat; body: {body}"
    );
    assert!(
        body.contains("rating") || body.contains("5"),
        "Response must include the dead property value; body: {body}"
    );
}
