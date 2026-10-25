use super::*;

async fn reply(
    connection: &Arc<Connection>,
    receiver: &mut mpsc::Receiver<Vec<u8>>,
    case: u32,
    body: &[u8],
) -> (u32, Vec<u8>) {
    assert!(connection.handle(&rpc(1, 99, false, case, body)).await);
    let bytes = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let response = parse(&bytes).unwrap().unwrap();
    assert!(response.response);
    assert_eq!(response.request_id, 99);
    (response.case, response.body.to_vec())
}

#[tokio::test]
async fn upload_cancel_and_resume_keep_partial_bytes_without_duplicate_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::new(dir.path().join("files"));
    let connection = Connection::new(
        storage,
        Arc::new(AtomicU64::new(1)),
        CancellationToken::new(),
    );
    let mut output = connection.take_receiver().unwrap();
    let id = Id { task: 7, file: 0 };
    let info = FileInfo {
        path: "test.bin".into(),
        size: 6,
        modified: 0,
    };
    let mut start = id.payload();
    wire::put_bytes(2, b"/files", &mut start);
    wire::put_bytes(3, &info.encode(), &mut start);
    wire::put_varint(4, 4, &mut start);
    assert_eq!(reply(&connection, &mut output, 1, &start).await.0, 1);
    let id = Id { task: 7, file: 1 };
    let ask = status(id.payload(), 3, 6);
    let (case, body) = reply(&connection, &mut output, 3, &ask).await;
    assert_eq!(case, 3);
    assert_eq!(number(&wire::decode(&body).unwrap(), 3), OK);
    let mut block = id.payload();
    wire::put_varint(2, 0, &mut block);
    wire::put_bytes(3, b"abc", &mut block);
    reply(&connection, &mut output, 4, &block).await;
    reply(&connection, &mut output, 4, &block).await;
    assert_eq!(
        std::fs::read(dir.path().join("files/test.bin")).unwrap(),
        b"abc"
    );
    reply(
        &connection,
        &mut output,
        9,
        &status(id.payload(), 2, CANCELED),
    )
    .await;
    assert!(connection.uploads.lock().await.is_empty());
    reply(&connection, &mut output, 1, &start).await;
    let (_, body) = reply(&connection, &mut output, 3, &ask).await;
    assert_eq!(number(&wire::decode(&body).unwrap(), 4), 3);
    let mut block = id.payload();
    wire::put_varint(2, 0, &mut block);
    wire::put_bytes(3, b"def", &mut block);
    let (_, body) = reply(&connection, &mut output, 4, &block).await;
    assert_eq!(number(&wire::decode(&body).unwrap(), 3), OK);
    reply(&connection, &mut output, 9, &status(id.payload(), 2, OK)).await;
    assert_eq!(
        std::fs::read(dir.path().join("files/test.bin")).unwrap(),
        b"abcdef"
    );
    assert!(connection.uploads.lock().await.is_empty());
    connection.close().await;
}

fn fixture() -> (tempfile::TempDir, Arc<Connection>, mpsc::Receiver<Vec<u8>>) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::new(dir.path().join("files"));
    let connection = Connection::new(
        storage,
        Arc::new(AtomicU64::new(1)),
        CancellationToken::new(),
    );
    let output = connection.take_receiver().unwrap();
    (dir, connection, output)
}

fn manifest(task: u64, name: &str, size: u64, strategy: u64) -> Vec<u8> {
    let mut body = Id { task, file: 0 }.payload();
    wire::put_bytes(2, b"/files", &mut body);
    wire::put_bytes(
        3,
        &FileInfo {
            path: name.into(),
            size,
            modified: 0,
        }
        .encode(),
        &mut body,
    );
    wire::put_varint(4, strategy, &mut body);
    body
}

#[tokio::test]
async fn invalid_names_sizes_and_compression_bombs_do_not_create_transfer_tasks() {
    let (dir, connection, mut output) = fixture();
    for (name, size) in [
        ("../outside", 1),
        ("/absolute", 1),
        ("a/../../outside", 1),
        ("large", MAX_FILE_BYTES + 1),
    ] {
        let (_, body) = reply(&connection, &mut output, 1, &manifest(8, name, size, 1)).await;
        assert_eq!(number(&wire::decode(&body).unwrap(), 3), PROTOCOL);
        assert!(connection.uploads.lock().await.is_empty());
    }
    let mut bomb = Id { task: 9, file: 0 }.payload();
    wire::put_bytes(2, b"/files", &mut bomb);
    wire::put_bytes(
        6,
        &compress(&vec![0; MAX_LIST_BYTES + 1]).unwrap(),
        &mut bomb,
    );
    let (_, body) = reply(&connection, &mut output, 1, &bomb).await;
    assert_eq!(number(&wire::decode(&body).unwrap(), 3), PROTOCOL);
    assert!(!dir.path().join("outside").exists());
    assert_eq!(
        std::fs::read_dir(dir.path().join("files")).unwrap().count(),
        0
    );
    connection.close().await;
}

#[tokio::test]
async fn out_of_order_conflicting_and_oversize_blocks_cannot_corrupt_the_file() {
    let (dir, connection, mut output) = fixture();
    reply(
        &connection,
        &mut output,
        1,
        &manifest(11, "bounded.bin", 5, 1),
    )
    .await;
    let id = Id { task: 11, file: 1 };
    reply(&connection, &mut output, 3, &status(id.payload(), 3, 5)).await;
    for (block, data, expected_len, expected_code) in [
        (2, b"abc".as_slice(), 0, PROTOCOL),
        (0, b"abcdef", 0, PROTOCOL),
        (0, b"abc", 3, OK),
        (0, b"xyz", 3, PROTOCOL),
        (1, b"def", 3, PROTOCOL),
        (1, b"de", 5, OK),
    ] {
        let mut body = id.payload();
        wire::put_varint(2, block, &mut body);
        wire::put_bytes(3, data, &mut body);
        let (_, response) = reply(&connection, &mut output, 4, &body).await;
        assert_eq!(number(&wire::decode(&response).unwrap(), 3), expected_code);
        assert_eq!(
            std::fs::metadata(dir.path().join("files/bounded.bin"))
                .unwrap()
                .len(),
            expected_len
        );
    }
    reply(&connection, &mut output, 9, &status(id.payload(), 2, OK)).await;
    assert_eq!(
        std::fs::read(dir.path().join("files/bounded.bin")).unwrap(),
        b"abcde"
    );
    connection.close().await;
}

#[tokio::test]
async fn download_resumes_at_the_confirmed_offset_and_requires_correlated_block_acks() {
    let (dir, connection, mut output) = fixture();
    let root = dir.path().join("files");
    std::fs::create_dir_all(&root).unwrap();
    let content: Vec<_> = (0..80000).map(|n| (n * 37 % 251) as u8).collect();
    std::fs::write(root.join("roundtrip.bin"), &content).unwrap();
    let id = Id { task: 16, file: 0 };
    let mut request = id.payload();
    wire::put_bytes(2, b"/files/roundtrip.bin", &mut request);
    let (case, body) = reply(&connection, &mut output, 2, &request).await;
    assert_eq!(case, 2);
    let fields = wire::decode(&body).unwrap();
    assert_eq!(number(&fields, 2), OK);
    let list = inflate(bytes(&fields, 5)).unwrap();
    let list = wire::decode(&list).unwrap();
    assert_eq!(repeated(&list, 1).count(), 1);
    let mut downloaded = Vec::new();
    loop {
        let bytes = tokio::time::timeout(Duration::from_secs(2), output.recv())
            .await
            .unwrap()
            .unwrap();
        let message = parse(&bytes).unwrap().unwrap();
        assert!(!message.response);
        let fields = wire::decode(message.body).unwrap();
        let id = Id::parse(&fields).unwrap();
        if message.case == 9 && id.file == u64::MAX {
            assert_eq!(number(&fields, 2), OK);
            break;
        }
        let response_case = match message.case {
            3 => 3,
            4 => {
                downloaded.extend_from_slice(protocol::bytes(&fields, 3));
                4
            }
            9 => 6,
            _ => panic!("Unexpected download request"),
        };
        let mut response = status(id.payload(), if response_case == 6 { 2 } else { 3 }, OK);
        if message.case == 3 {
            wire::put_varint(4, 1025, &mut response);
        }
        if message.case == 4 {
            wire::put_varint(2, number(&fields, 2), &mut response);
            wire::put_varint(4, protocol::bytes(&fields, 3).len() as u64, &mut response);
        }
        // An unrelated reply must not consume this pending request.
        connection
            .handle(&rpc(
                1,
                message.request_id + 10000,
                true,
                response_case,
                &response,
            ))
            .await;
        assert!(connection.pending.lock().contains_key(&message.request_id));
        connection
            .handle(&rpc(1, message.request_id, true, response_case, &response))
            .await;
    }
    assert_eq!(downloaded, content[1025..]);
    connection.close().await;
}

#[tokio::test]
async fn cancel_download_wakes_an_unacknowledged_block_and_clears_pending_requests() {
    let (dir, connection, mut output) = fixture();
    std::fs::create_dir_all(dir.path().join("files")).unwrap();
    std::fs::write(dir.path().join("files/cancel.bin"), vec![4; 100000]).unwrap();
    let mut request = Id { task: 20, file: 0 }.payload();
    wire::put_bytes(2, b"/files/cancel.bin", &mut request);
    reply(&connection, &mut output, 2, &request).await;
    let bytes = output.recv().await.unwrap();
    let ask = parse(&bytes).unwrap().unwrap();
    let id = Id::parse(&wire::decode(ask.body).unwrap()).unwrap();
    connection
        .handle(&rpc(
            1,
            ask.request_id,
            true,
            3,
            &status(id.payload(), 3, OK),
        ))
        .await;
    let bytes = output.recv().await.unwrap();
    let block = parse(&bytes).unwrap().unwrap();
    assert_eq!(block.case, 4);
    assert!(!connection.pending.lock().is_empty());
    reply(
        &connection,
        &mut output,
        9,
        &status(id.payload(), 2, CANCELED),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while !connection.pending.lock().is_empty() || !connection.downloads.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    connection.close().await;
}

#[tokio::test]
async fn disconnect_closes_upload_fds_even_if_file_channel_never_opened() {
    let (dir, connection, mut output) = fixture();
    reply(&connection, &mut output, 1, &manifest(25, "held.bin", 3, 1)).await;
    reply(
        &connection,
        &mut output,
        3,
        &status(Id { task: 25, file: 1 }.payload(), 3, 3),
    )
    .await;
    let location = Location::parse("/files/held.bin").unwrap().unwrap();
    let access = connection.storage.access(&location).await.unwrap();
    assert!(access.open_file(&location, true).is_err());
    connection.closed.cancel();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !connection.uploads.lock().await.is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    access.open_file(&location, true).unwrap();
    assert_eq!(
        std::fs::metadata(dir.path().join("files/held.bin"))
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn timed_out_rpc_does_not_leave_a_pending_reply_slot() {
    let (_dir, connection, mut output) = fixture();
    let sending = connection.clone();
    let task = tokio::spawn(async move {
        sending
            .request(
                3,
                &Id { task: 35, file: 1 }.payload(),
                &CancellationToken::new(),
            )
            .await
    });
    output.recv().await.unwrap();
    assert_eq!(connection.pending.lock().len(), 1);
    tokio::time::advance(Duration::from_secs(21)).await;
    assert!(matches!(task.await.unwrap(), Err(IO)));
    assert!(connection.pending.lock().is_empty());
    connection.close().await;
}

#[tokio::test]
async fn duplicate_skip_and_invalid_resume_preserve_the_existing_file() {
    let (dir, connection, mut output) = fixture();
    let root = dir.path().join("files");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("keep.bin"), b"original").unwrap();
    for (task, strategy, code) in [(40, 3, OK), (41, 4, PROTOCOL), (42, 2, OK)] {
        reply(
            &connection,
            &mut output,
            1,
            &manifest(task, "keep.bin", 3, strategy),
        )
        .await;
        let id = Id { task, file: 1 };
        let (_, body) = reply(&connection, &mut output, 3, &status(id.payload(), 3, 3)).await;
        let fields = wire::decode(&body).unwrap();
        assert_eq!(number(&fields, 3), code);
        if strategy == 3 {
            assert_eq!(number(&fields, 2), 1);
            assert!(!connection.uploads.lock().await.contains_key(&task));
        }
        if strategy == 2 {
            let mut block = id.payload();
            wire::put_varint(2, 0, &mut block);
            wire::put_bytes(3, b"new", &mut block);
            reply(&connection, &mut output, 4, &block).await;
            reply(&connection, &mut output, 9, &status(id.payload(), 2, OK)).await;
            assert_eq!(std::fs::read(root.join("keep (1).bin")).unwrap(), b"new");
        }
        assert_eq!(std::fs::read(root.join("keep.bin")).unwrap(), b"original");
    }
    connection.close().await;
}
