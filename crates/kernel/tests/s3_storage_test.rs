#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `S3Storage` against the in-process mock S3 server (`common::mock_s3`): put,
//! get (streamed), delete, exists, size, a presigned GET URL, and that two
//! backends with different key prefixes (as `public`/`private` get via
//! `file::build_storage`) cannot see each other's objects.
//!
//! No database needed — this is pure backend behavior.

mod common;

use std::time::Duration;

use common::mock_s3::{self, MockS3};
use trovato_kernel::file::{FileStorage, S3Settings, S3Storage};

fn settings(mock: &MockS3, key_prefix: &str) -> S3Settings {
    S3Settings {
        endpoint: mock.endpoint(),
        region: mock_s3::REGION.to_string(),
        bucket: mock_s3::BUCKET.to_string(),
        key_prefix: key_prefix.to_string(),
        path_style: true,
        access_key_id: mock_s3::ACCESS_KEY_ID.to_string(),
        secret_access_key: mock_s3::SECRET_ACCESS_KEY.to_string(),
        public_base_url: None,
        presign_ttl: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn writes_reads_sizes_and_deletes_a_round_trip() {
    let mock = MockS3::start().await;
    let storage = S3Storage::new(settings(&mock, "")).unwrap();

    let uri = "s3://2026/01/abc_photo.jpg";
    storage.write(uri, b"hello s3").await.unwrap();

    assert!(storage.exists(uri).await.unwrap());
    assert_eq!(storage.read(uri).await.unwrap(), b"hello s3".to_vec());
    assert_eq!(storage.size(uri).await.unwrap(), 8);

    storage.delete(uri).await.unwrap();
    assert!(!storage.exists(uri).await.unwrap());
    assert!(storage.read(uri).await.is_err(), "deleted object reads as an error");
}

#[tokio::test]
async fn read_stream_yields_the_same_bytes_as_read() {
    use tokio_stream::StreamExt;

    let mock = MockS3::start().await;
    let storage = S3Storage::new(settings(&mock, "")).unwrap();
    let uri = "s3://streamed.bin";
    let payload = vec![7u8; 500_000];
    storage.write(uri, &payload).await.unwrap();

    let mut stream = storage.read_stream(uri).await.unwrap();
    let mut collected = Vec::new();
    while let Some(chunk) = stream.next().await {
        collected.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(collected, payload);
}

#[tokio::test]
async fn a_presigned_url_fetches_the_object_with_no_authorization_header() {
    let mock = MockS3::start().await;
    let storage = S3Storage::new(settings(&mock, "")).unwrap();
    let uri = "s3://private/report.pdf";
    storage.write(uri, b"confidential").await.unwrap();

    let url = storage
        .presigned_get_url(uri)
        .await
        .unwrap()
        .expect("S3 backend supports presigning");

    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"confidential");
}

#[tokio::test]
async fn an_anonymous_get_with_no_signature_is_refused() {
    let mock = MockS3::start().await;
    let storage = S3Storage::new(settings(&mock, "")).unwrap();
    let uri = "s3://secret.txt";
    storage.write(uri, b"nope").await.unwrap();

    // A plain unsigned GET, as any caller without the credentials would send.
    let url = format!(
        "{}/{}/secret.txt",
        mock.endpoint(),
        mock_s3::BUCKET
    );
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn two_backends_with_different_prefixes_do_not_see_each_others_objects() {
    let mock = MockS3::start().await;
    // Mirrors how `file::build_storage` separates the public and private
    // schemes sharing one bucket: `{prefix}public/`, `{prefix}private/`.
    let public = S3Storage::new(settings(&mock, "public/")).unwrap();
    let private = S3Storage::new(settings(&mock, "private/")).unwrap();

    let uri = "s3://2026/01/same_relative_path.jpg";
    public.write(uri, b"public bytes").await.unwrap();
    private.write(uri, b"private bytes").await.unwrap();

    assert_eq!(public.read(uri).await.unwrap(), b"public bytes".to_vec());
    assert_eq!(private.read(uri).await.unwrap(), b"private bytes".to_vec());
    assert_eq!(mock.object_count(), 2, "distinct keys on the wire");
    assert_eq!(
        mock.object("public/2026/01/same_relative_path.jpg").unwrap(),
        b"public bytes"
    );
    assert_eq!(
        mock.object("private/2026/01/same_relative_path.jpg").unwrap(),
        b"private bytes"
    );
}

#[tokio::test]
async fn describe_names_the_bucket_and_never_the_secret() {
    let mock = MockS3::start().await;
    let storage = S3Storage::new(settings(&mock, "")).unwrap();
    let description = storage.describe();
    assert!(description.contains(mock_s3::BUCKET));
    assert!(!description.contains(mock_s3::SECRET_ACCESS_KEY));
}

#[tokio::test]
async fn probe_round_trips_a_sentinel_object() {
    let mock = MockS3::start().await;
    let storage = S3Storage::new(settings(&mock, "")).unwrap();
    storage.probe().await.unwrap();
    assert_eq!(mock.object_count(), 0, "the probe cleans up after itself");
}
