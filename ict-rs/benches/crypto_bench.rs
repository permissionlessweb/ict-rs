use criterion::{black_box, criterion_group, criterion_main, Criterion};
use ict_rs::auth::{generate_mnemonic, Authenticator, KeyringAuthenticator};
use ict_rs::wallet::KeyWallet;

fn bench_mnemonic_generation(c: &mut Criterion) {
    c.bench_function("generate_mnemonic_24_word", |b| {
        b.iter(|| {
            let m = generate_mnemonic();
            black_box(m);
        });
    });
}

fn bench_key_derivation(c: &mut Criterion) {
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    c.bench_function("keyring_authenticator_new", |b| {
        b.iter(|| {
            let auth = KeyringAuthenticator::new(black_box(mnemonic), 118).unwrap();
            black_box(auth);
        });
    });
}

fn bench_public_key(c: &mut Criterion) {
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let auth = KeyringAuthenticator::new(mnemonic, 118).unwrap();

    c.bench_function("public_key_bytes", |b| {
        b.iter(|| {
            let pk = auth.public_key_bytes();
            black_box(pk);
        });
    });
}

fn bench_address_derivation(c: &mut Criterion) {
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let auth = KeyringAuthenticator::new(mnemonic, 118).unwrap();

    c.bench_function("address_bytes_sha256_ripemd160", |b| {
        b.iter(|| {
            let addr = auth.address_bytes();
            black_box(addr);
        });
    });

    c.bench_function("bech32_address_encoding", |b| {
        b.iter(|| {
            let addr = auth.bech32_address(black_box("cosmos")).unwrap();
            black_box(addr);
        });
    });
}

fn bench_signing(c: &mut Criterion) {
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let auth = KeyringAuthenticator::new(mnemonic, 118).unwrap();
    let sign_doc = b"test sign document for benchmarking ecdsa signing performance";

    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("ecdsa_sign_prehash", |b| {
        b.iter(|| {
            let sig = rt.block_on(auth.sign(black_box(sign_doc))).unwrap();
            black_box(sig);
        });
    });
}

fn bench_wallet_from_mnemonic(c: &mut Criterion) {
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    c.bench_function("key_wallet_from_mnemonic", |b| {
        b.iter(|| {
            let w = KeyWallet::from_mnemonic(
                black_box("test"),
                black_box(mnemonic),
                black_box("cosmos"),
                118,
            )
            .unwrap();
            black_box(w);
        });
    });
}

fn bench_different_coin_types(c: &mut Criterion) {
    let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    let mut group = c.benchmark_group("key_derivation_coin_types");
    for coin_type in [118u32, 60, 330, 529] {
        group.bench_with_input(
            format!("coin_type_{coin_type}"),
            &coin_type,
            |b, &ct| {
                b.iter(|| {
                    let auth = KeyringAuthenticator::new(black_box(mnemonic), ct).unwrap();
                    black_box(auth);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_mnemonic_generation,
    bench_key_derivation,
    bench_public_key,
    bench_address_derivation,
    bench_signing,
    bench_wallet_from_mnemonic,
    bench_different_coin_types,
);
criterion_main!(benches);
