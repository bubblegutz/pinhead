//! Edge-case tests: reconnection, multiple clients, concurrent reads.

mod common;
use common::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const SCRIPT: &str = include_str!("../examples/basic/main.lua");

fn raw_ninep_sock(sock: &str) -> Result<NinepClient, String> {
    let mut client = NinepClient::connect_unix(sock)?;
    setup_client(&mut client)?;
    Ok(client)
}

#[test]
fn basic_read() {
    let id = unique_id();
    let sock = format!("/tmp/pinhead-e2e-base-{:x}.sock", id);
    let t = Transport::NinepSock(sock);
    let mut inst = PinheadInstance::start(SCRIPT, &t).expect("start");
    let mut client = inst.connect().expect("connect");
    let text = client.read_file("testfile.txt").expect("read");
    assert_eq!(text, "hello from pinhead test!");
}

#[test]
fn ninep_reconnect() {
    let id = unique_id();
    let sock = format!("/tmp/pinhead-e2e-recon-{:x}.sock", id);
    let t = Transport::NinepSock(sock.clone());
    let _inst = PinheadInstance::start(SCRIPT, &t).expect("start");

    for round in 0..3 {
        let mut c = raw_ninep_sock(&sock).expect("connect");
        let text = c.read_file("testfile.txt").expect("read");
        assert_eq!(text, "hello from pinhead test!", "round {round}");
    }
}

#[test]
fn multiple_clients() {
    let id = unique_id();
    let sock = format!("/tmp/pinhead-e2e-multi-{:x}.sock", id);
    let t = Transport::NinepSock(sock.clone());
    let _inst = PinheadInstance::start(SCRIPT, &t).expect("start");

    let n_clients = 8;
    let mut clients = Vec::new();
    for _ in 0..n_clients {
        clients.push(raw_ninep_sock(&sock).expect("connect"));
    }

    for (i, client) in clients.iter_mut().enumerate() {
        let text = client.read_file("testfile.txt").expect("read");
        assert_eq!(text, "hello from pinhead test!", "client {i}");
    }
}

#[test]
fn concurrent_reads() {
    let id = unique_id();
    let sock = format!("/tmp/pinhead-e2e-concur-{:x}.sock", id);
    let t = Transport::NinepSock(sock.clone());
    let _inst = PinheadInstance::start(SCRIPT, &t).expect("start");

    let n_threads = 4;
    let reads_per = 10;
    let ok_count = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for _ in 0..n_threads {
        let ok = ok_count.clone();
        let a = sock.clone();
        handles.push(std::thread::spawn(move || {
            let mut client = raw_ninep_sock(&a).expect("connect");
            for _ in 0..reads_per {
                if let Ok(text) = client.read_file("testfile.txt")
                    && text == "hello from pinhead test!" {
                        ok.fetch_add(1, Ordering::SeqCst);
                    }
            }
        }));
    }

    for h in handles {
        h.join().expect("thread panicked");
    }

    let total = n_threads * reads_per;
    let passed = ok_count.load(Ordering::SeqCst);
    assert_eq!(passed, total, "{passed}/{total} concurrent reads succeeded");
}
