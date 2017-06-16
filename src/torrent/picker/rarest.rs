// Implementation based off of http://blog.libtorrent.org/2011/11/writing-a-fast-piece-picker/

use std::collections::{HashSet, HashMap};
use torrent::{Bitfield, Info, Peer};
use std::ops::IndexMut;

pub struct Picker {
    /// Bitfield of which blocks have been waiting
    blocks: Bitfield,
    /// Number of blocks per piece
    scale: u64,
    /// Set of pieces which have blocks waiting. These should be prioritized.
    waiting: HashSet<u64>,
    /// Map of block indeces to peers waiting on them. Used for
    /// cancelling in endgame.
    waiting_peers: HashMap<u64, HashSet<usize>>,
    /// Number of blocks left to request. Once this becomes 0
    /// endgame mode is entered.
    endgame_cnt: u64,
    /// Current order of pieces
    pieces: Vec<u32>,
    /// Indices into pieces which indicate priority bounds
    priorities: Vec<usize>,
    /// Index mapping a piece to a position in the pieces field
    piece_idx: Vec<PieceInfo>,
    /// Set of peers which are seeders, and are not included in availability calcs
    seeders: HashSet<usize>,
}

struct PieceInfo {
    idx: usize,
    availability: usize,
}

impl Picker {
    pub fn new(info: &Info) -> Picker {
        let scale = info.piece_len/16384;
        // The n - 1 piece length, since the last one is (usually) shorter.
        let compl_piece_len = scale * (info.pieces() as usize - 1);
        // the nth piece length
        let mut last_piece_len = info.total_len - info.piece_len as u64 * (info.pieces() as u64 - 1) as u64;
        if last_piece_len % 16384 == 0 {
            last_piece_len /= 16384;
        } else {
            last_piece_len /= 16384;
            last_piece_len += 1;
        }
        let len = compl_piece_len + last_piece_len as usize;
        let blocks = Bitfield::new(len as u64);
        let mut piece_idx = Vec::new();
        for i in 0..info.pieces() {
            piece_idx.push(PieceInfo { idx: i as usize, availability: 0 });
        }
        Picker {
            blocks,
            scale: scale as u64,
            endgame_cnt: len as u64,
            waiting_peers: HashMap::new(),
            waiting: HashSet::new(),
            seeders: HashSet::new(),
            pieces: (0..info.pieces()).collect(),
            piece_idx,
            priorities: vec![info.pieces() as usize],
        }
    }

    pub fn add_peer(&mut self, peer: &Peer) {
        // Ignore seeders for availability calc
        if peer.pieces.complete() {
            self.seeders.insert(peer.id);
            return;
        }
        for idx in peer.pieces.iter() {
            self.piece_available(idx as u32);
        }
    }

    pub fn remove_peer(&mut self, peer: &Peer) {
        if self.seeders.contains(&peer.id) {
            self.seeders.remove(&peer.id);
            return;
        }
        for idx in peer.pieces.iter() {
            self.piece_unavailable(idx as u32);
        }
    }

    pub fn piece_available(&mut self, piece: u32) {
        let (idx, avail) = {
            let piece = self.piece_idx.index_mut(piece as usize);
            self.priorities[piece.availability] -= 1;
            piece.availability += 1;
            if self.priorities.len() == piece.availability {
                self.priorities.push(self.pieces.len());
            }
            (piece.idx, piece.availability - 1)
        };

        let swap_idx = self.priorities[avail];
        let swap_piece = self.pieces[swap_idx];

        {
            let piece = self.piece_idx.index_mut(piece as usize);
            piece.idx = swap_idx;
        }
        {
            let piece = self.piece_idx.index_mut(swap_piece as usize);
            piece.idx = idx;
        }
        self.pieces.swap(idx, swap_idx);
    }

    pub fn piece_unavailable(&mut self, piece: u32) {
        let (idx, avail) = {
            let piece = self.piece_idx.index_mut(piece as usize);
            piece.availability -= 1;
            self.priorities[piece.availability] += 1;
            (piece.idx, piece.availability)
        };

        let swap_idx = self.priorities[avail - 1];
        let swap_piece = self.pieces[swap_idx];

        {
            let piece = self.piece_idx.index_mut(piece as usize);
            piece.idx = swap_idx;
        }
        {
            let piece = self.piece_idx.index_mut(swap_piece as usize);
            piece.idx = idx;
        }
        self.pieces.swap(idx, swap_idx);
    }

    pub fn pick(&mut self, peer: &Peer) -> Option<(u32, u32)> {
        for pidx in self.pieces.iter() {
            if peer.pieces.has_bit(*pidx as u64) {
                for bidx in 0..self.scale {
                    let block = *pidx as u64 * self.scale + bidx;
                    if !self.blocks.has_bit(block) {
                        self.blocks.set_bit(block);
                        let mut hs = HashSet::with_capacity(1);
                        hs.insert(peer.id);
                        self.waiting_peers.insert(block, hs);
                        self.waiting.insert(block);
                        if self.endgame_cnt == 1 {
                            println!("Entering endgame!");
                        }
                        self.endgame_cnt = self.endgame_cnt.saturating_sub(1);
                        return Some((*pidx as u32, bidx as u32 * 16384));
                    }
                }
            }
        }
        if self.endgame_cnt == 0 {
            let mut idx = None;
            for piece in self.waiting.iter() {
                if peer.pieces.has_bit(*piece/self.scale) {
                    idx = Some(*piece);
                    break;
                }
            }
            if let Some(i) = idx {
                self.waiting_peers.get_mut(&i).unwrap();
                return Some(((i/self.scale) as u32, ((i % self.scale) * 16384) as u32));
            }
        }
        None
    }

    pub fn completed(&mut self, oidx: u32, offset: u32) -> (bool, HashSet<usize>) {
        let idx: u64 = oidx as u64 * self.scale;
        let offset: u64 = offset as u64/16384;
        let block = idx + offset;
        self.waiting.remove(&block);
        let peers = self.waiting_peers.remove(&block).unwrap_or(HashSet::with_capacity(0));
        for i in 0..self.scale {
            if (idx + i < self.blocks.len() && !self.blocks.has_bit(idx + i)) || self.waiting.contains(&(idx + i)) {
                return (false, peers);
            }
        }

        // TODO: Make this less hacky somehow
        // let pri_idx = self.piece_idx[oidx as usize].availability;
        // let pinfo_idx = self.piece_idx[oidx as usize].idx;
        // for pri in self.priorities.iter_mut() {
        //     if *pri > pri_idx as usize {
        //         *pri -= 1;
        //     }
        // }
        // for pinfo in self.piece_idx.iter_mut() {
        //     if pinfo.idx > pinfo_idx {
        //         pinfo.idx -= 1;
        //     }
        // }
        // self.pieces.remove(pinfo_idx);
        for _ in 0..100 {
            self.piece_available(oidx);
        }
        (true, peers)
    }
}

#[test]
fn test_available() {
    use socket::Socket;
    let info = Info {
        name: String::from(""),
        announce: String::from(""),
        piece_len: 16384,
        total_len: 16384 * 3,
        hashes: vec![vec![0u8]; 3],
        hash: [0u8; 20],
        files: vec![],
    };

    let mut picker = Picker::new(&info);
    let mut peers = vec![Peer::new(Socket::empty()), Peer::new(Socket::empty()), Peer::new(Socket::empty())];
    for peer in peers.iter_mut() {
        peer.pieces = Bitfield::new(3);
    }
    assert_eq!(picker.pick(&peers[0]), None);

    peers[0].pieces.set_bit(0);
    peers[1].pieces.set_bit(0);
    peers[1].pieces.set_bit(2);
    peers[2].pieces.set_bit(1);

    for peer in peers.iter() {
        picker.add_peer(peer);
    }
    assert_eq!(picker.pick(&peers[1]), Some((2, 0)));
    assert_eq!(picker.pick(&peers[1]), Some((0, 0)));
    assert_eq!(picker.pick(&peers[1]), None);
    assert_eq!(picker.pick(&peers[0]), None);
    assert_eq!(picker.pick(&peers[2]), Some((1, 0)));
}

#[test]
fn test_unavailable() {
    use socket::Socket;
    let info = Info {
        name: String::from(""),
        announce: String::from(""),
        piece_len: 16384,
        total_len: 16384 * 3,
        hashes: vec![vec![0u8]; 3],
        hash: [0u8; 20],
        files: vec![],
    };

    let mut picker = Picker::new(&info);
    let mut peers = vec![Peer::new(Socket::empty()), Peer::new(Socket::empty()), Peer::new(Socket::empty())];
    for peer in peers.iter_mut() {
        peer.pieces = Bitfield::new(3);
    }
    assert_eq!(picker.pick(&peers[0]), None);

    peers[0].pieces.set_bit(0);
    peers[0].pieces.set_bit(1);
    peers[1].pieces.set_bit(1);
    peers[1].pieces.set_bit(2);
    peers[2].pieces.set_bit(0);
    peers[2].pieces.set_bit(1);

    for peer in peers.iter() {
        picker.add_peer(peer);
    }
    picker.remove_peer(&peers[0]);

    assert_eq!(picker.pick(&peers[1]), Some((2, 0)));
    assert_eq!(picker.pick(&peers[2]), Some((0, 0)));
    assert_eq!(picker.pick(&peers[2]), Some((1, 0)));
    assert_eq!(picker.pick(&peers[1]), None);
}