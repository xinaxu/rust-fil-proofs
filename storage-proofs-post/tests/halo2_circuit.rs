use std::convert::TryInto;
use std::marker::PhantomData;

use filecoin_hashers::{poseidon::PoseidonHasher, HashFunction, Hasher, PoseidonArity};
use generic_array::typenum::{U0, U2, U8};
use halo2_proofs::{arithmetic::FieldExt, dev::MockProver, pasta::Fp};
use rand::SeedableRng;
use rand_xorshift::XorShiftRng;
use storage_proofs_core::{
    halo2::CircuitRows,
    merkle::{generate_tree, DiskTree, MerkleProofTrait, MerkleTreeTrait},
    TEST_SEED,
};
use storage_proofs_post::halo2::{
    constants::{SECTOR_NODES_16_KIB, SECTOR_NODES_2_KIB, SECTOR_NODES_32_KIB, SECTOR_NODES_4_KIB},
    window, winning, SectorProof, WindowPostCircuit, WinningPostCircuit,
};
use tempfile::tempdir;

pub type TreeR<F, U, V, W> = DiskTree<PoseidonHasher<F>, U, V, W>;

fn test_winning_post_circuit<F, U, V, W, const SECTOR_NODES: usize>()
where
    F: FieldExt,
    U: PoseidonArity<F>,
    V: PoseidonArity<F>,
    W: PoseidonArity<F>,
    PoseidonHasher<F>: Hasher<Field = F>,
{
    let sector_id = 0u64;
    let k = 0;

    let mut rng = XorShiftRng::from_seed(TEST_SEED);

    let randomness = F::random(&mut rng);

    let temp_dir = tempdir().expect("tempdir failure");
    let temp_path = temp_dir.path();
    let (replica, tree_r) = generate_tree::<TreeR<F, U, V, W>, _>(
        &mut rng,
        SECTOR_NODES,
        Some(temp_path.to_path_buf()),
    );

    let root_r = tree_r.root();
    let comm_c = F::random(&mut rng);
    let comm_r = <PoseidonHasher<F> as Hasher>::Function::hash2(&comm_c.into(), &root_r);

    let challenges = winning::generate_challenges::<F, SECTOR_NODES>(randomness, sector_id, k);

    let leafs_r = challenges
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let c = *c as usize;
            let start = c << 5;
            let leaf_bytes = &replica[start..start + 32];
            let mut repr = F::Repr::default();
            repr.as_mut().copy_from_slice(leaf_bytes);
            let leaf = F::from_repr_vartime(repr).unwrap_or_else(|| {
                panic!("leaf bytes are not a valid field element for c_{}={}", i, c)
            });
            Some(leaf)
        })
        .collect::<Vec<Option<F>>>()
        .try_into()
        .unwrap();

    let paths_r = challenges
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let c = *c as usize;
            let merkle_proof = tree_r
                .gen_proof(c)
                .unwrap_or_else(|_| panic!("failed to generate merkle proof for c_{}={}", i, c));
            merkle_proof
                .path()
                .iter()
                .map(|(siblings, _)| siblings.iter().map(|&sib| Some(sib.into())).collect())
                .collect::<Vec<Vec<Option<F>>>>()
        })
        .collect::<Vec<Vec<Vec<Option<F>>>>>()
        .try_into()
        .unwrap();

    let pub_inputs = winning::PublicInputs::<F, SECTOR_NODES> {
        comm_r: Some(comm_r.into()),
        challenges: challenges
            .iter()
            .copied()
            .map(Some)
            .collect::<Vec<Option<u32>>>()
            .try_into()
            .unwrap(),
    };
    let pub_inputs_vec = pub_inputs.to_vec();

    let priv_inputs = winning::PrivateInputs::<F, U, V, W, SECTOR_NODES> {
        comm_c: Some(comm_c),
        root_r: Some(root_r.into()),
        leafs_r,
        paths_r,
        _tree_r: PhantomData,
    };

    let circ = WinningPostCircuit {
        pub_inputs,
        priv_inputs,
    };

    let prover = MockProver::run(circ.k(), &circ, pub_inputs_vec).unwrap();
    assert!(prover.verify().is_ok());
}

#[test]
fn test_winning_post_circuit_2kib_halo2() {
    test_winning_post_circuit::<Fp, U8, U0, U0, SECTOR_NODES_2_KIB>()
}

#[test]
fn test_winning_post_circuit_4kib_halo2() {
    test_winning_post_circuit::<Fp, U8, U2, U0, SECTOR_NODES_4_KIB>()
}

#[test]
fn test_winning_post_circuit_16kib_halo2() {
    test_winning_post_circuit::<Fp, U8, U8, U0, SECTOR_NODES_16_KIB>()
}

#[test]
fn test_winning_post_circuit_32kib_halo2() {
    test_winning_post_circuit::<Fp, U8, U8, U2, SECTOR_NODES_32_KIB>()
}

fn test_window_post_circuit<F, U, V, W, const SECTOR_NODES: usize>()
where
    F: FieldExt,
    U: PoseidonArity<F>,
    V: PoseidonArity<F>,
    W: PoseidonArity<F>,
    PoseidonHasher<F>: Hasher<Field = F>,
{
    let challenged_sector_count = window::sectors_challenged_per_partition::<SECTOR_NODES>();
    let k = 0;

    let mut rng = XorShiftRng::from_seed(TEST_SEED);

    let randomness = F::random(&mut rng);

    let temp_dir = tempdir().expect("tempdir failure");
    let temp_path = temp_dir.path().to_path_buf();

    let mut pub_inputs = window::PublicInputs::<F, SECTOR_NODES> {
        comms_r: Vec::with_capacity(challenged_sector_count),
        challenges: Vec::with_capacity(challenged_sector_count),
    };

    let mut priv_inputs = window::PrivateInputs::<F, U, V, W, SECTOR_NODES> {
        sector_proofs: Vec::with_capacity(challenged_sector_count),
    };

    for sector_index in 0..challenged_sector_count {
        let sector_id = sector_index as u64;

        let (replica, tree_r) =
            generate_tree::<TreeR<F, U, V, W>, _>(&mut rng, SECTOR_NODES, Some(temp_path.clone()));

        let root_r = tree_r.root();
        let comm_c = F::random(&mut rng);
        let comm_r = <PoseidonHasher<F> as Hasher>::Function::hash2(&comm_c.into(), &root_r);

        let challenges =
            window::generate_challenges::<F, SECTOR_NODES>(randomness, sector_index, sector_id, k);

        pub_inputs.comms_r.push(Some(comm_r.into()));
        pub_inputs.challenges.push(
            challenges
                .iter()
                .copied()
                .map(Some)
                .collect::<Vec<Option<u32>>>()
                .try_into()
                .unwrap(),
        );

        let leafs_r = challenges
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let c = *c as usize;
                let start = c << 5;
                let leaf_bytes = &replica[start..start + 32];
                let mut repr = F::Repr::default();
                repr.as_mut().copy_from_slice(leaf_bytes);
                let leaf = F::from_repr_vartime(repr).unwrap_or_else(|| {
                    panic!(
                        "leaf bytes are not a valid field element for c_{}={} (sector_{})",
                        i, c, sector_index,
                    )
                });
                Some(leaf)
            })
            .collect::<Vec<Option<F>>>()
            .try_into()
            .unwrap();

        let paths_r = challenges
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let c = *c as usize;
                let merkle_proof = tree_r.gen_proof(c).unwrap_or_else(|_| {
                    panic!(
                        "failed to generate merkle proof for c_{}={} (sector_{})",
                        i, c, sector_index,
                    )
                });
                merkle_proof
                    .path()
                    .iter()
                    .map(|(siblings, _)| siblings.iter().map(|&sib| Some(sib.into())).collect())
                    .collect::<Vec<Vec<Option<F>>>>()
            })
            .collect::<Vec<Vec<Vec<Option<F>>>>>()
            .try_into()
            .unwrap();

        priv_inputs.sector_proofs.push(SectorProof {
            comm_c: Some(comm_c),
            root_r: Some(root_r.into()),
            leafs_r,
            paths_r,
            _tree_r: PhantomData,
        });
    }

    let pub_inputs_vec = pub_inputs.to_vec();

    let circ = WindowPostCircuit {
        pub_inputs,
        priv_inputs,
    };

    let prover = MockProver::run(circ.k(), &circ, pub_inputs_vec).unwrap();
    assert!(prover.verify().is_ok());
}

#[test]
fn test_window_post_circuit_2kib_halo2() {
    test_window_post_circuit::<Fp, U8, U0, U0, SECTOR_NODES_2_KIB>()
}

#[test]
fn test_window_post_circuit_4kib_halo2() {
    test_window_post_circuit::<Fp, U8, U2, U0, SECTOR_NODES_4_KIB>()
}

#[test]
fn test_window_post_circuit_16kib_halo2() {
    test_window_post_circuit::<Fp, U8, U8, U0, SECTOR_NODES_16_KIB>()
}

#[test]
fn test_window_post_circuit_32kib_halo2() {
    test_window_post_circuit::<Fp, U8, U8, U2, SECTOR_NODES_32_KIB>()
}
