//! The proof both ends of Windows' pipe give before a URL crosses it (K64,
//! D-131): each shows it can read the token the listening Kerno wrote in the
//! data directory, without sending the token.
//!
//! The client sends a fresh nonce; the instance answers its own nonce and
//! SHA-256(token ‖ client nonce ‖ "server"); the client checks it, then
//! sends SHA-256(token ‖ instance nonce ‖ "client") before its line, which
//! the instance checks before reading the line. Fresh nonces on both sides:
//! an answer recorded once proves nothing the next time; the side's name in
//! the digest: one side's proof is never the other's. Digests are compared
//! with `==`, not in constant time (the tree has no helper for it within
//! reach; a local peer timing a 32-byte comparison learns too little).
//!
//! Pure functions, tested on every system; only Windows' pipe uses them.

use sha2::{Digest as _, Sha256};

/// The size of the token, of a nonce and of a proof.
pub(super) const SIZE: usize = 32;

/// A token, a nonce or a proof.
pub(super) type Bytes = [u8; SIZE];

fn digest(token: &Bytes, nonce: &Bytes, side: &[u8]) -> Bytes {
    Sha256::new()
        .chain_update(token)
        .chain_update(nonce)
        .chain_update(side)
        .finalize()
        .into()
}

/// What the instance answers a client's nonce with: its own nonce, then its
/// proof.
pub(super) fn server_answer(
    token: &Bytes,
    client_nonce: &Bytes,
    server_nonce: &Bytes,
) -> [u8; 2 * SIZE] {
    let mut answer = [0; 2 * SIZE];
    answer[..SIZE].copy_from_slice(server_nonce);
    answer[SIZE..].copy_from_slice(&digest(token, client_nonce, b"server"));
    answer
}

/// The instance's nonce, when `answer` proves it holds `token`; `None`
/// otherwise, a short or long answer included.
pub(super) fn check_server(token: &Bytes, client_nonce: &Bytes, answer: &[u8]) -> Option<Bytes> {
    if answer.len() != 2 * SIZE {
        return None;
    }
    let (nonce, proof) = answer.split_at(SIZE);
    (proof == digest(token, client_nonce, b"server").as_slice())
        .then(|| nonce.try_into().ok())
        .flatten()
}

/// The client's proof, for the instance's nonce.
pub(super) fn client_proof(token: &Bytes, server_nonce: &Bytes) -> Bytes {
    digest(token, server_nonce, b"client")
}

/// Whether `proof` shows that the client holds `token`.
pub(super) fn check_client(token: &Bytes, server_nonce: &Bytes, proof: &[u8]) -> bool {
    proof == client_proof(token, server_nonce).as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the holder of the token proves anything, each proof for its own
    /// nonce and its own side. Mutation seen: `check_server` and
    /// `check_client` always passing.
    #[test]
    fn only_the_token_s_holder_proves_itself_once() {
        let (token, other) = ([1; SIZE], [2; SIZE]);
        let (client_nonce, server_nonce, older) = ([3; SIZE], [4; SIZE], [5; SIZE]);

        // The right token: both checks pass, and the instance's nonce comes back.
        let answer = server_answer(&token, &client_nonce, &server_nonce);
        assert_eq!(
            check_server(&token, &client_nonce, &answer),
            Some(server_nonce)
        );
        let proof = client_proof(&token, &server_nonce);
        assert!(check_client(&token, &server_nonce, &proof));

        // A wrong token, on either side.
        let forged = server_answer(&other, &client_nonce, &server_nonce);
        assert_eq!(check_server(&token, &client_nonce, &forged), None);
        assert!(!check_client(
            &token,
            &server_nonce,
            &client_proof(&other, &server_nonce)
        ));

        // Replayed: an answer to another client's nonce, a proof for an
        // older instance nonce.
        let replayed = server_answer(&token, &older, &server_nonce);
        assert_eq!(check_server(&token, &client_nonce, &replayed), None);
        assert!(!check_client(
            &token,
            &server_nonce,
            &client_proof(&token, &older)
        ));

        // One side's proof is not the other's.
        let reflected = digest(&token, &server_nonce, b"server");
        assert!(!check_client(&token, &server_nonce, &reflected));

        // Truncated or padded.
        assert_eq!(
            check_server(&token, &client_nonce, &answer[..2 * SIZE - 1]),
            None
        );
        assert_eq!(
            check_server(&token, &client_nonce, &[answer.as_slice(), &[0]].concat()),
            None
        );
        assert!(!check_client(&token, &server_nonce, &proof[..SIZE - 1]));
        assert!(!check_client(&token, &server_nonce, &[]));
    }
}
