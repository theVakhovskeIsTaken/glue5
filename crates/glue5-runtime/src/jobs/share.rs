//! The lobby's jobs: each player's go over to the others by themselves.
//!
//! Every few seconds a player announces the jobs it made or got (an id, a
//! fingerprint of the file, its size) on a port of the lobby's virtual LAN
//! that the engine does not use; a player without one (or with another
//! version) asks the announcer for its pieces, a few at a time, again when
//! a piece does not come. Received whole and matching its fingerprint, the
//! file goes to `jobs/shared/` and into the library: when someone hosts a
//! job, the others have it already, as the scripts expect (they find a
//! job's header and its file at once).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{Job, Kind, MAX_FILE, add, fingerprint, is_content_id, jobs, parse_job, shared_dir};

/// The port on the virtual LAN (the engine's sockets take 6672 and 50000 up).
pub(crate) const PORT: u16 = 47900;
const BROADCAST: u32 = u32::MAX;
const ANNOUNCE: u8 = b'A';
const GET: u8 = b'G';
const PIECE: u8 = b'P';
/// A piece's size, inside a datagram.
const PIECE_SIZE: usize = 1024;
/// Jobs in one announcement: (22 + 8 + 4) bytes each.
const PER_ANNOUNCEMENT: usize = 32;
const ANNOUNCE_EVERY: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);
/// Pieces asked for at a time, and when one is asked for again.
const WINDOW: usize = 16;
const RETRY: Duration = Duration::from_millis(600);
/// A download whose announcer says nothing for this long is dropped.
const GIVE_UP: Duration = Duration::from_secs(30);

struct Download {
    hash: [u8; 8],
    from: u32,
    pieces: Vec<Option<Vec<u8>>>,
    asked: Vec<Option<Instant>>,
    heard: Instant,
}

static DOWNLOADS: Mutex<Option<HashMap<String, Download>>> = Mutex::new(None);

pub(super) fn start() {
    let started = std::thread::Builder::new().name("jobs".into()).spawn(|| {
        let mut announced = Instant::now() - ANNOUNCE_EVERY;
        loop {
            std::thread::sleep(TICK);
            if crate::p2p::get().is_none() {
                continue;
            }
            if announced.elapsed() >= ANNOUNCE_EVERY {
                announced = Instant::now();
                announce();
            }
            ask();
        }
    });
    if let Err(e) = started {
        log::warn!("jobs: sharing: {e}");
    }
}

fn send(to: u32, payload: &[u8]) {
    if let Some(p2p) = crate::p2p::get() {
        p2p.send(PORT, to, PORT, payload);
    }
}

/// The jobs the lobby gets from this player: its own and those it got.
fn shareable() -> Vec<Job> {
    jobs().into_iter().filter(|j| j.kind != Kind::Game && j.size as usize <= MAX_FILE).collect()
}

fn announce() {
    for chunk in shareable().chunks(PER_ANNOUNCEMENT) {
        let mut d = vec![ANNOUNCE];
        for job in chunk {
            d.extend_from_slice(job.id.as_bytes());
            d.extend_from_slice(&job.hash);
            d.extend_from_slice(&job.size.to_be_bytes());
        }
        send(BROADCAST, &d);
    }
}

/// Asks for the pieces missing, a window at a time.
fn ask() {
    let now = Instant::now();
    let mut downloads = DOWNLOADS.lock().unwrap();
    let Some(downloads) = downloads.as_mut() else { return };
    downloads.retain(|id, d| {
        if now.duration_since(d.heard) > GIVE_UP {
            log::info!("jobs: {id} from {}: gave up", std::net::Ipv4Addr::from(d.from));
            return false;
        }
        true
    });
    for (id, d) in downloads.iter_mut() {
        let waiting = d.asked.iter().zip(&d.pieces).filter(|(a, p)| p.is_none() && a.is_some_and(|t| now.duration_since(t) < RETRY)).count();
        let mut room = WINDOW.saturating_sub(waiting);
        for i in 0..d.pieces.len() {
            if room == 0 {
                break;
            }
            if d.pieces[i].is_some() || d.asked[i].is_some_and(|t| now.duration_since(t) < RETRY) {
                continue;
            }
            let mut g = vec![GET];
            g.extend_from_slice(id.as_bytes());
            g.extend_from_slice(&d.hash);
            g.extend_from_slice(&(i as u32).to_be_bytes());
            send(d.from, &g);
            d.asked[i] = Some(now);
            room -= 1;
        }
    }
}

/// A datagram on the jobs' port, from `from` (vnet.rs).
pub(crate) fn receive(from: u32, d: &[u8]) {
    match d.first() {
        Some(&ANNOUNCE) => on_announce(from, &d[1..]),
        Some(&GET) if d.len() >= 1 + 22 + 8 + 4 => {
            let (id, hash) = (&d[1..23], &d[23..31]);
            let index = u32::from_be_bytes(d[31..35].try_into().unwrap()) as usize;
            on_get(from, id, hash, index);
        }
        Some(&PIECE) if d.len() >= 1 + 22 + 8 + 8 => {
            let (id, hash) = (&d[1..23], &d[23..31]);
            let index = u32::from_be_bytes(d[31..35].try_into().unwrap()) as usize;
            let count = u32::from_be_bytes(d[35..39].try_into().unwrap()) as usize;
            on_piece(from, id, hash, index, count, &d[39..]);
        }
        _ => {}
    }
}

fn on_announce(from: u32, d: &[u8]) {
    let have = jobs();
    let mut downloads = DOWNLOADS.lock().unwrap();
    let downloads = downloads.get_or_insert_default();
    for e in d.as_chunks::<34>().0 {
        let Ok(id) = std::str::from_utf8(&e[..22]) else { continue };
        let hash: [u8; 8] = e[22..30].try_into().unwrap();
        let size = u32::from_be_bytes(e[30..34].try_into().unwrap()) as usize;
        if !is_content_id(id) || size == 0 || size > MAX_FILE {
            continue;
        }
        // The player's own jobs stay its own; a version already here stays.
        if have.iter().any(|j| j.id == id && (j.kind == Kind::Own || j.hash == hash)) {
            continue;
        }
        match downloads.get_mut(id) {
            Some(d) if d.hash == hash => d.heard = Instant::now(),
            _ => {
                let count = size.div_ceil(PIECE_SIZE);
                log::info!("jobs: {id} ({size} bytes) from {}", std::net::Ipv4Addr::from(from));
                downloads.insert(
                    id.to_owned(),
                    Download { hash, from, pieces: vec![None; count], asked: vec![None; count], heard: Instant::now() },
                );
            }
        }
    }
}

fn on_get(from: u32, id: &[u8], hash: &[u8], index: usize) {
    let Ok(id) = std::str::from_utf8(id) else { return };
    let Some(job) = shareable().into_iter().find(|j| j.id == id && j.hash == hash) else { return };
    // A file of a few hundred KB at most, read again for each piece: the
    // disk's cache keeps it.
    let Ok(data) = std::fs::read(&job.path) else { return };
    if fingerprint(&data) != job.hash {
        return;
    }
    let count = data.len().div_ceil(PIECE_SIZE);
    let Some(piece) = data.chunks(PIECE_SIZE).nth(index) else { return };
    let mut p = vec![PIECE];
    p.extend_from_slice(id.as_bytes());
    p.extend_from_slice(hash);
    p.extend_from_slice(&(index as u32).to_be_bytes());
    p.extend_from_slice(&(count as u32).to_be_bytes());
    p.extend_from_slice(piece);
    send(from, &p);
}

fn on_piece(from: u32, id: &[u8], hash: &[u8], index: usize, count: usize, piece: &[u8]) {
    let Ok(id) = std::str::from_utf8(id) else { return };
    let mut guard = DOWNLOADS.lock().unwrap();
    let Some(downloads) = guard.as_mut() else { return };
    let Some(d) = downloads.get_mut(id) else { return };
    if d.hash != hash || d.from != from || count != d.pieces.len() || index >= count {
        return;
    }
    d.heard = Instant::now();
    d.pieces[index] = Some(piece.to_vec());
    if d.pieces.iter().any(Option::is_none) {
        return;
    }
    let d = downloads.remove(id).unwrap();
    drop(guard);
    let data: Vec<u8> = d.pieces.into_iter().flatten().flatten().collect();
    if fingerprint(&data) != d.hash {
        log::warn!("jobs: {id} from {}: not what was announced", std::net::Ipv4Addr::from(from));
        return;
    }
    let dir = shared_dir();
    let path = dir.join(format!("{id}.json"));
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, &data)) {
        log::warn!("jobs: saving {}: {e}", path.display());
        return;
    }
    if let Some(job) = parse_job(&data, id, Kind::Shared, "", &path) {
        add(job);
    }
}

// An announcement and a piece fit a datagram the Internet carries whole.
const _: () = assert!(1 + PER_ANNOUNCEMENT * 34 < 1200 && 1 + 22 + 8 + 8 + PIECE_SIZE < 1200);
