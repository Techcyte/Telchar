//! Tests gateway-store substitution through the typed EnsurePath worker operation.

use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::thread;

use nix_worker_protocol::{CLIENT_WORKER_MAGIC, SERVER_WORKER_MAGIC, STDERR_LAST};
use telchar::store::substitution::{GatewayStoreSubstitution, StoreSubstitutionBackend};

#[test]
fn ensure_path_uses_configured_gateway_daemon() {
    let root =
        std::env::temp_dir().join(format!("telchar-store-substitution-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir(&root).expect("fixture root creates");
    let socket = root.join("daemon.sock");
    let listener = UnixListener::bind(&socket).expect("fixture daemon binds");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("fixture daemon accepts");
        assert_eq!(read_integer(&mut stream), CLIENT_WORKER_MAGIC);
        let client_version = read_integer(&mut stream);
        write_integer(&mut stream, SERVER_WORKER_MAGIC);
        write_integer(&mut stream, client_version);
        stream.flush().expect("server version flushes");
        assert_eq!(read_integer(&mut stream), 0);
        write_integer(&mut stream, 0);
        stream.flush().expect("feature negotiation flushes");
        assert_eq!(read_integer(&mut stream), 0);
        assert_eq!(read_integer(&mut stream), 0);
        write_string(&mut stream, b"2.34.8");
        write_integer(&mut stream, 1);
        write_integer(&mut stream, STDERR_LAST);
        stream.flush().expect("post-handshake flushes");
        assert_eq!(read_integer(&mut stream), 10);
        assert_eq!(
            read_string(&mut stream),
            b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cached"
        );
        write_integer(&mut stream, STDERR_LAST);
        write_integer(&mut stream, 1);
        stream.flush().expect("operation flushes");
    });
    let endpoint =
        telchar::store::GatewayStoreEndpoint::parse(&format!("unix://{}", socket.display()))
            .expect("endpoint parses");
    let mut substitution = GatewayStoreSubstitution::new(endpoint);

    substitution
        .ensure_path(b"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-cached")
        .expect("path substitutes");

    server.join().expect("fixture daemon joins");
    std::fs::remove_dir_all(root).expect("fixture removes");
}

fn read_integer(input: &mut impl Read) -> u64 {
    let mut value = [0_u8; 8];
    input.read_exact(&mut value).expect("integer reads");
    u64::from_le_bytes(value)
}

fn write_integer(output: &mut impl Write, value: u64) {
    output
        .write_all(&value.to_le_bytes())
        .expect("integer writes");
}

fn read_string(input: &mut impl Read) -> Vec<u8> {
    let length = read_integer(input) as usize;
    let mut value = vec![0_u8; length];
    input.read_exact(&mut value).expect("string reads");
    let mut padding = vec![0_u8; (8 - length % 8) % 8];
    input.read_exact(&mut padding).expect("padding reads");
    value
}

fn write_string(output: &mut impl Write, value: &[u8]) {
    write_integer(output, value.len() as u64);
    output.write_all(value).expect("string writes");
    output
        .write_all(&vec![0_u8; (8 - value.len() % 8) % 8])
        .expect("padding writes");
}
