//! Hashes a term list for shipping (§20.5.1): plain terms on stdin, one
//! per line, and the hashed list on stdout. Each term is normalized the
//! way torrent text is, and every token it yields is kept, so a term
//! matches however it's written. Keep the plain list out of the
//! repository.
//!
//! `cargo run -p lodestar-filter --example hash_terms < terms.txt`

use std::io::Read;

use lodestar_filter::{TermSet, normalize};

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("reading stdin");
    let mut set = TermSet::default();
    for line in input.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut tokens = Vec::new();
        normalize::tokens(line, &mut tokens);
        for t in tokens.iter().filter(|t| t.num.is_none()) {
            set.insert(t.hash);
        }
    }
    print!("{}", set.to_list());
}
