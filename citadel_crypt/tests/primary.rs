#[cfg(test)]
mod tests {
    use bytes::{BufMut, BytesMut};
    use citadel_crypt::argon::argon_container::{
        ArgonSettings, ArgonStatus, AsyncArgon, ServerArgonContainer,
    };
    use citadel_crypt::endpoint_crypto_container::EndpointRatchetConstructor;
    use citadel_crypt::entropy_bank::{EntropyBank, SecurityLevel};
    use citadel_crypt::misc::TransferType;
    use citadel_crypt::packet_vector::PacketVector;
    use citadel_crypt::scramble::crypt_splitter::{par_scramble_encrypt_group, GroupReceiver};
    use citadel_crypt::secure_buffer::sec_bytes::SecBuffer;
    use citadel_crypt::secure_buffer::sec_string::SecString;
    use citadel_crypt::stacked_ratchet::{Ratchet, StackedRatchet};
    use citadel_crypt::toolset::{Toolset, UpdateStatus, MAX_HYPER_RATCHETS_IN_MEMORY};
    use citadel_pqcrypto::algorithm_dictionary::{
        AlgorithmsExt, CryptoParameters, EncryptionAlgorithm, KemAlgorithm, SigAlgorithm,
        KEM_ALGORITHM_COUNT,
    };
    use citadel_pqcrypto::constructor_opts::ConstructorOpts;
    use rstest::rstest;
    #[cfg(not(target_family = "wasm"))]
    use std::path::PathBuf;

    #[cfg(not(target_family = "wasm"))]
    #[tokio::test]
    async fn argon_autotuner() {
        use citadel_crypt::argon::autotuner::calculate_optimal_argon_params;
        citadel_logging::setup_log();
        let start_time = std::time::Instant::now();
        let final_cfg = calculate_optimal_argon_params(500_u16, Some(32), None)
            .await
            .unwrap();
        log::trace!(target: "citadel", "DONE. Elapsed time: {:?}", start_time.elapsed());
        log::trace!(target: "citadel", "{:?}", final_cfg)
    }

    #[tokio::test]
    async fn argon() {
        citadel_logging::setup_log();

        // Client config should be a weaker version than the server version, since the client doesn't actually store the password on their own device. Still, if login time can in total be kept under 2s, then it's good
        let client_config = ArgonSettings::new_gen_salt(
            "Thomas P Braun".as_bytes().to_vec(),
            8,
            32,
            1024 * 64,
            4,
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0],
        );
        // client hashes their password
        match AsyncArgon::hash(SecBuffer::from("password"), client_config.clone())
            .await
            .unwrap()
        {
            ArgonStatus::HashSuccess(hashed_password) => {
                log::trace!(target: "citadel", "Hash success!");
                // now, the client stores the config in their CNAC to be able to hash again in the future. Next, client sends the hashed password through an encrypted stream to the server
                let server_recv = hashed_password;
                // The server creates their own version of the settings, which should be dependent on the capabilities of that server. (Aim for 0.5s < x < 1.0s hash time)
                let server_config = ArgonSettings::new_gen_salt(
                    "Thomas P Braun".as_bytes().to_vec(),
                    8,
                    32,
                    1024 * 64,
                    4,
                    vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0],
                );
                // the server then hashes the server_recv
                match AsyncArgon::hash(server_recv.clone(), server_config.clone())
                    .await
                    .unwrap()
                {
                    ArgonStatus::HashSuccess(hashed_password_x2) => {
                        // The server saves this hashed output to the backend. Then, if a client wants to login, they have to hash their password
                        let server_argon_container =
                            ServerArgonContainer::new(server_config, hashed_password_x2.clone());

                        match AsyncArgon::hash(SecBuffer::from("password"), client_config.clone())
                            .await
                            .unwrap()
                        {
                            ArgonStatus::HashSuccess(hashed_password_v2) => {
                                //assert_eq!(hashed_password_v2.as_ref(), server_recv.as_ref());
                                // client sends to server to verify
                                match AsyncArgon::verify(
                                    hashed_password_v2,
                                    server_argon_container.clone(),
                                )
                                .await
                                .unwrap()
                                {
                                    ArgonStatus::VerificationSuccess => {
                                        log::trace!(target: "citadel", "Verification success!");
                                        return;
                                    }

                                    n => {
                                        log::error!(target: "citadel", "{:?}", n);
                                    }
                                }
                            }

                            n => {
                                log::error!(target: "citadel", "{:?}", n);
                            }
                        }
                    }

                    n => {
                        log::error!(target: "citadel", "{:?}", n);
                    }
                }
            }

            n => {
                log::error!(target: "citadel", "{:?}", n);
            }
        }

        panic!("Failed somewhere");
    }

    #[test]
    fn test_sec_buffer() {
        let buf = SecBuffer::from("Hello, world!");
        let serde = bincode2::serialize(&buf).unwrap();
        std::mem::drop(buf);
        let buf = bincode2::deserialize::<SecBuffer>(&serde).unwrap();

        assert_eq!(buf.as_ref(), b"Hello, world!");
        let cloned = buf.clone();
        let ptr = cloned.as_ref().as_ptr();
        let len = cloned.as_ref().len();
        let ptr_slice = unsafe { std::slice::from_raw_parts(ptr, len) };

        assert_eq!(cloned.as_ref(), ptr_slice);
        let retrieved = buf.into_buffer();

        assert_eq!(&*retrieved, b"Hello, world!");
    }

    #[test]
    fn test_sec_string() {
        let mut val = SecString::new();
        assert_eq!(val.len(), 0);
        val.push('h');
        val.push('e');
        //val.clear();
        let mut basic = val.clone();
        assert_eq!(val.len(), 2);
        assert_eq!(basic.len(), 2);
        assert_eq!(basic.as_str(), "he");

        basic.push('y');
        assert_ne!(val.as_str(), basic.as_str());

        let retrieved = basic.into_buffer();
        let serde = bincode2::serialize(&retrieved).unwrap();
        let retrieved = bincode2::deserialize::<SecString>(&serde)
            .unwrap()
            .into_buffer();
        // at this point, basic should have dropped, but the memory should not have been zeroed out
        assert_eq!(retrieved, "hey");
    }
    /*
    #[test]
    fn onion_packets() {
        onion_packet::<StackedRatchet>();
        #[cfg(feature = "fcm")]
            onion_packet::<citadel_crypt::fcm::fcm_ratchet::FcmRatchet>();
    }

    fn onion_packet<R: Ratchet>() {
        citadel_logging::setup_log();
        const LEN: usize = 5;
        const HEADER_LEN: usize = 50;
        let message = "Hello, world!";
        let algo = KemAlgorithm::Kyber + EncryptionAlgorithm::ChaCha20Poly_1305;

        let chain = CryptoRelayChain::<R>::from_iter((0..LEN).into_iter().map(|_idx| rand::random::<u64>())
            .map(|cid| {
                let mut alice_hr = R::Constructor::new_alice(vec![ConstructorOpts::new_init(Some(algo))], 0, 0, None);
                let transfer = alice_hr.stage0_alice();
                let bob_hr = R::Constructor::new_bob(0, 0, vec![ConstructorOpts::new_init(Some(algo))], transfer).unwrap();
                let transfer = bob_hr.stage0_bob().unwrap();
                alice_hr.stage1_alice(&transfer).unwrap();
                let toolset = Toolset::new(cid, alice_hr.finish().unwrap());
                let container = PeerSessionCrypto::new(toolset, true);
                container
            }));

        log::trace!(target: "citadel", "Generated chain!");

        let onion_packet = chain.encrypt(message, 0, HEADER_LEN, |_ratchet, _target_cid, buffer| {
            for x in 0..HEADER_LEN {
                buffer.put_u8(x as u8);
            }
        }).unwrap();

        println!("Onion packet: {:?}", &onion_packet);
        let cids_order_decrypt = chain.target_cid_list.as_ref().unwrap().iter().rev().cloned().collect::<Vec<u64>>();
        println!("{:?}\n", &cids_order_decrypt);
        let output = chain.links.iter().rfold(onion_packet, |mut acc, (cid, container)| {
            println!("At {} (onion packet len: {})", cid, acc.len());
            let (pqc, drill) = container.get_hyper_ratchet(None).unwrap().message_pqc_drill(None);
            let payload = acc.split_off(HEADER_LEN);
            drill.aes_gcm_decrypt(0, pqc, payload)
                .map(|vec| bytes::BytesMut::from(&vec[..])).unwrap()
        });

        assert_eq!(message, String::from_utf8(output.to_vec()).unwrap());
    }*/

    #[test]
    fn secstring() {
        citadel_logging::setup_log();
        let mut val = SecString::new();
        assert_eq!(val.len(), 0);
        val.push('h');
        val.push('e');
        //val.clear();
        let mut basic = val.clone();
        assert_eq!(val.len(), 2);
        assert_eq!(basic.len(), 2);
        assert_eq!(basic.as_str(), "he");

        basic.push('y');
        assert_ne!(val.as_str(), basic.as_str());

        let retrieved = basic.into_buffer();
        let serde = bincode2::serialize(&retrieved).unwrap();
        let retrieved = bincode2::deserialize::<SecString>(&serde)
            .unwrap()
            .into_buffer();
        // at this point, basic should have dropped, but the memory should not have been zeroed out
        assert_eq!(retrieved, "hey");
    }

    #[test]
    fn secbytes() {
        citadel_logging::setup_log();
        let buf = SecBuffer::from("Hello, world!");
        let serde = bincode2::serialize(&buf).unwrap();
        std::mem::drop(buf);
        let buf = bincode2::deserialize::<SecBuffer>(&serde).unwrap();

        assert_eq!(buf.as_ref(), b"Hello, world!");
        let cloned = buf.clone();
        let ptr = cloned.as_ref().as_ptr();
        let len = cloned.as_ref().len();
        let ptr_slice = unsafe { std::slice::from_raw_parts(ptr, len) };

        assert_eq!(cloned.as_ref(), ptr_slice);
        let retrieved = buf.into_buffer();

        assert_eq!(&*retrieved, b"Hello, world!");
    }

    #[test]
    fn hyper_ratchets() {
        citadel_logging::setup_log();
        for x in 0u8..KEM_ALGORITHM_COUNT {
            for sec in 0..SecurityLevel::Extreme.value() {
                let _ = hyper_ratchet::<StackedRatchet, _>(
                    KemAlgorithm::from_u8(x).unwrap() + EncryptionAlgorithm::AES_GCM_256,
                    Some(sec.into()),
                    false,
                );
                let _ = hyper_ratchet::<StackedRatchet, _>(
                    KemAlgorithm::from_u8(x).unwrap() + EncryptionAlgorithm::ChaCha20Poly_1305,
                    Some(sec.into()),
                    false,
                );
            }
        }
    }

    #[test]
    fn hyper_ratchets_fcm() {
        citadel_logging::setup_log();
        for x in 0u8..KEM_ALGORITHM_COUNT {
            for sec in 0..SecurityLevel::Extreme.value() {
                let _ = hyper_ratchet::<citadel_crypt::fcm::fcm_ratchet::ThinRatchet, _>(
                    KemAlgorithm::from_u8(x).unwrap() + EncryptionAlgorithm::AES_GCM_256,
                    Some(sec.into()),
                    true,
                );
                let _ = hyper_ratchet::<citadel_crypt::fcm::fcm_ratchet::ThinRatchet, _>(
                    KemAlgorithm::from_u8(x).unwrap() + EncryptionAlgorithm::ChaCha20Poly_1305,
                    Some(sec.into()),
                    true,
                );
            }
        }
    }

    #[test]
    fn security_levels() {
        citadel_logging::setup_log();
        for sec in 0..SecurityLevel::Extreme.value() {
            let ratchet = hyper_ratchet::<StackedRatchet, _>(
                KemAlgorithm::Kyber + EncryptionAlgorithm::AES_GCM_256,
                Some(sec.into()),
                false,
            );
            for x in 0..sec {
                assert!(ratchet.verify_level(Some(x.into())).is_ok())
            }

            for x in (sec + 1)..SecurityLevel::Custom(255).value() {
                assert!(ratchet.verify_level(Some(x.into())).is_err())
            }
        }
    }

    fn hyper_ratchet<R: Ratchet, Z: Into<CryptoParameters>>(
        algorithm: Z,
        security_level: Option<SecurityLevel>,
        is_fcm: bool,
    ) -> R {
        let algorithm = algorithm.into();
        log::trace!(target: "citadel", "Using {:?} with {:?} @ {:?} security level | is FCM: {}", algorithm.kem_algorithm, algorithm.encryption_algorithm, security_level, is_fcm);
        let algorithm = Some(algorithm);
        let count = (security_level.unwrap_or_default().value() + 1) as usize;
        let mut alice_hyper_ratchet = R::Constructor::new_alice(
            ConstructorOpts::new_vec_init(algorithm, count),
            99,
            0,
            security_level,
        )
        .unwrap();
        let transfer = alice_hyper_ratchet.stage0_alice().unwrap();

        let bob_hyper_ratchet = R::Constructor::new_bob(
            99,
            0,
            ConstructorOpts::new_vec_init(algorithm, count),
            transfer,
        )
        .unwrap();
        let transfer = bob_hyper_ratchet.stage0_bob().unwrap();

        alice_hyper_ratchet.stage1_alice(transfer).unwrap();

        let alice_hyper_ratchet = alice_hyper_ratchet.finish().unwrap();
        let bob_hyper_ratchet = bob_hyper_ratchet.finish().unwrap();

        const MESSAGE: &[u8] = b"Hello, world!" as &[u8];
        const HEADER_LEN: usize = 50;

        let mut packet = BytesMut::with_capacity(MESSAGE.len() + HEADER_LEN);

        for x in 0..50 {
            packet.put_u8(x);
        }

        packet.put(MESSAGE);

        let plaintext_packet = packet.clone();

        alice_hyper_ratchet
            .protect_message_packet(security_level, HEADER_LEN, &mut packet)
            .unwrap();
        assert_ne!(packet, plaintext_packet);

        let mut header = packet.split_to(HEADER_LEN);
        bob_hyper_ratchet
            .validate_message_packet(security_level, &header[..], &mut packet)
            .unwrap();

        header.unsplit(packet);

        assert_eq!(header, plaintext_packet);
        alice_hyper_ratchet
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn toolsets(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        toolset::<StackedRatchet>(enx, kem, sig);
        #[cfg(feature = "fcm")]
        toolset::<citadel_crypt::fcm::fcm_ratchet::ThinRatchet>(enx, kem, sig);
    }

    fn toolset<R: Ratchet>(enx: EncryptionAlgorithm, kem: KemAlgorithm, sig: SigAlgorithm) {
        citadel_logging::setup_log();
        const COUNT: u32 = 100;
        let security_level = SecurityLevel::Standard;

        let (alice, _bob) = gen::<R>(0, 0, security_level, enx + kem + sig);

        let mut toolset = Toolset::new(0, alice);

        for x in 1..COUNT {
            let res = toolset
                .update_from(gen::<R>(0, x, security_level, enx + kem + sig).0)
                .unwrap();
            match res {
                UpdateStatus::Committed { .. } => {
                    assert!(x < MAX_HYPER_RATCHETS_IN_MEMORY as u32);
                    assert_eq!(0, toolset.get_oldest_hyper_ratchet_version());
                    assert_eq!(x, toolset.get_most_recent_hyper_ratchet_version());
                }

                UpdateStatus::CommittedNeedsSynchronization { old_version, .. } => {
                    assert_eq!(old_version, 0); // we're not truncating it yet, so it should be 0
                    assert!(x + 1 > MAX_HYPER_RATCHETS_IN_MEMORY as u32);
                    assert_eq!(0, toolset.get_oldest_hyper_ratchet_version()); // this shouldn't change because the oldest needs to be manually removed
                    assert_eq!(x, toolset.get_most_recent_hyper_ratchet_version());
                }
            }
        }

        for x in 0..COUNT {
            if toolset.deregister_oldest_hyper_ratchet(x).is_ok() {
                assert_eq!(x + 1, toolset.get_oldest_hyper_ratchet_version());
            } else {
                assert_eq!(toolset.len(), MAX_HYPER_RATCHETS_IN_MEMORY);
                assert_eq!(
                    toolset.get_oldest_hyper_ratchet_version(),
                    COUNT - MAX_HYPER_RATCHETS_IN_MEMORY as u32
                );
            }
        }

        let _res = toolset
            .update_from(gen::<R>(0, COUNT, security_level, enx + kem + sig).0)
            .unwrap();
        assert_eq!(toolset.len(), MAX_HYPER_RATCHETS_IN_MEMORY + 1);
        assert_eq!(
            toolset.get_oldest_hyper_ratchet_version(),
            toolset.get_most_recent_hyper_ratchet_version() - MAX_HYPER_RATCHETS_IN_MEMORY as u32
        );

        toolset
            .deregister_oldest_hyper_ratchet(
                toolset.get_most_recent_hyper_ratchet_version()
                    - MAX_HYPER_RATCHETS_IN_MEMORY as u32,
            )
            .unwrap();
        assert_eq!(toolset.len(), MAX_HYPER_RATCHETS_IN_MEMORY);
    }

    fn gen<R: Ratchet>(
        cid: u64,
        version: u32,
        sec: SecurityLevel,
        algorithm: CryptoParameters,
    ) -> (R, R) {
        let count = sec.value() as usize + 1;
        let mut alice = R::Constructor::new_alice(
            ConstructorOpts::new_vec_init(Some(algorithm), count),
            cid,
            version,
            Some(sec),
        )
        .unwrap();
        let bob = R::Constructor::new_bob(
            cid,
            version,
            ConstructorOpts::new_vec_init(Some(algorithm), count),
            alice.stage0_alice().unwrap(),
        )
        .unwrap();
        let stage0_bob = bob.stage0_bob().unwrap();
        alice.stage1_alice(stage0_bob).unwrap();
        (alice.finish().unwrap(), bob.finish().unwrap())
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn toolset_wrapping_vers_all(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        toolset_wrapping_vers::<StackedRatchet>(enx, kem, sig);
        #[cfg(feature = "fcm")]
        toolset_wrapping_vers::<citadel_crypt::fcm::fcm_ratchet::ThinRatchet>(enx, kem, sig);
    }

    fn toolset_wrapping_vers<R: Ratchet>(
        enx: EncryptionAlgorithm,
        kem: KemAlgorithm,
        sig: SigAlgorithm,
    ) {
        citadel_logging::setup_log();
        let vers = u32::MAX - 1;
        let cid = 10;
        let hr = gen::<R>(cid, vers, SecurityLevel::Standard, enx + kem + sig);
        let mut toolset = Toolset::new_debug(cid, hr.0, vers, vers);
        let r = toolset.get_hyper_ratchet(vers).unwrap();
        assert_eq!(r.version(), vers);

        const COUNT: usize = 100;
        let mut insofar = 0;
        let mut cur_vers = vers.wrapping_add(1);
        loop {
            if insofar >= COUNT {
                break;
            }

            toolset
                .update_from(gen::<R>(cid, cur_vers, SecurityLevel::Standard, enx + kem + sig).0)
                .unwrap();
            let ratchet = toolset.get_hyper_ratchet(cur_vers).unwrap();
            assert_eq!(ratchet.version(), cur_vers);
            cur_vers = cur_vers.wrapping_add(1);
            insofar += 1;
        }

        assert_eq!(toolset.get_oldest_hyper_ratchet().unwrap().version(), vers);
        let mut amt_culled = 0;
        for _ in 0..COUNT {
            if toolset.len() == MAX_HYPER_RATCHETS_IN_MEMORY {
                continue;
            }
            toolset
                .deregister_oldest_hyper_ratchet(vers.wrapping_add(amt_culled))
                .unwrap();
            amt_culled += 1;
            assert_eq!(
                toolset.get_oldest_hyper_ratchet().unwrap().version(),
                vers.wrapping_add(amt_culled)
            );
        }
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn scrambler_transmission_length_spectrum(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        scrambler_transmission_spectrum::<StackedRatchet>(
            enx,
            kem,
            sig,
            TransferType::FileTransfer,
            |decrypted, plaintext, _, _| debug_assert_eq!(decrypted, plaintext),
        );
        #[cfg(feature = "fcm")]
        scrambler_transmission_spectrum::<citadel_crypt::fcm::fcm_ratchet::ThinRatchet>(
            enx,
            kem,
            sig,
            TransferType::FileTransfer,
            |decrypted, plaintext, _, _| debug_assert_eq!(decrypted, plaintext),
        );
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn scrambler_transmission_length_spectrum_remote(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        let tx_type = TransferType::RemoteEncryptedVirtualFilesystem {
            virtual_path: PathBuf::from("/"),
            security_level: SecurityLevel::Standard,
        };

        fn verifier<R: Ratchet>(decrypted: &[u8], plaintext: &[u8], sa_alice: &R, sa_bob: &R) {
            assert_ne!(decrypted, plaintext);
            let decrypted_real = sa_alice
                .local_decrypt(decrypted, SecurityLevel::Standard)
                .unwrap();
            assert_eq!(decrypted_real, plaintext);
            assert!(sa_bob
                .local_decrypt(decrypted, SecurityLevel::Standard)
                .is_err());
        }

        scrambler_transmission_spectrum::<StackedRatchet>(enx, kem, sig, tx_type, verifier);
        #[cfg(feature = "fcm")]
        scrambler_transmission_spectrum::<citadel_crypt::fcm::fcm_ratchet::ThinRatchet>(
            enx, kem, sig, tx_type, verifier,
        );
    }

    fn scrambler_transmission_spectrum<R: Ratchet>(
        enx: EncryptionAlgorithm,
        kem: KemAlgorithm,
        sig: SigAlgorithm,
        transfer_type: TransferType,
        verifier: impl for<'a> Fn(&'a [u8], &'a [u8], &R, &R),
    ) {
        citadel_logging::setup_log();

        const SECURITY_LEVEL: SecurityLevel = SecurityLevel::Standard;
        const HEADER_SIZE_BYTES: usize = 44;

        let mut data = BytesMut::with_capacity(1500);
        let (ratchet_alice, ratchet_bob) = gen::<R>(10, 0, SECURITY_LEVEL, enx + kem + sig);
        let (pseudo_static_aux_ratchet_alice, pseudo_static_aux_ratchet_bob) =
            gen::<R>(10, 0, SECURITY_LEVEL, enx + kem + sig);

        for x in 0..1500_usize {
            data.put_u8((x % 256) as u8);
            let input_data = &data[..];

            let mut scramble_transmitter =
                par_scramble_encrypt_group::<_, _, _, HEADER_SIZE_BYTES>(
                    input_data,
                    SECURITY_LEVEL,
                    &ratchet_alice,
                    &pseudo_static_aux_ratchet_alice,
                    HEADER_SIZE_BYTES,
                    0,
                    0,
                    0,
                    transfer_type.clone(),
                    |_vec, _drill, _target_cid, _, buffer| {
                        for x in 0..HEADER_SIZE_BYTES {
                            buffer.put_u8(x as u8)
                        }
                    },
                )
                .unwrap();

            let config = scramble_transmitter.get_receiver_config();
            let mut receiver = GroupReceiver::new(config.clone(), 0, 0);
            log::trace!(target: "citadel", "{:?}", &config);

            while let Some(mut packet) = scramble_transmitter.get_next_packet() {
                //log::trace!(target: "citadel", "Packet {} (wave id: {}) obtained and ready to transmit to receiver", packet.vector.true_sequence, packet.vector.wave_id);
                let packet_payload = packet.packet.split_off(HEADER_SIZE_BYTES);
                let _result = receiver.on_packet_received(
                    0,
                    packet.vector.true_sequence,
                    packet.vector.wave_id,
                    &ratchet_bob,
                    packet_payload,
                );
                //println!("Wave {} result: {:?}", packet.vector.wave_id, result);
            }

            let decrypted_descrambled_plaintext = receiver.finalize();
            verifier(
                &decrypted_descrambled_plaintext,
                input_data,
                &pseudo_static_aux_ratchet_alice,
                &pseudo_static_aux_ratchet_bob,
            )
        }
    }

    const HEADER_LEN: usize = 52;
    fn header_inscribe(_: &PacketVector, _: &EntropyBank, _: u32, _: u64, packet: &mut BytesMut) {
        for x in 0..HEADER_LEN {
            packet.put_u8((x % 255) as u8)
        }
    }

    #[cfg(feature = "filesystem")]
    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    #[tokio::test]
    async fn encrypt_decrypt_file_transfer(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        citadel_logging::setup_log();

        let (bytes, bytes_ret, _sa_alice, _sa_bob) = test_file_transfer_inner(
            TransferType::FileTransfer,
            enx,
            kem,
            sig,
            Default::default(),
        )
        .await;
        assert_eq!(bytes, bytes_ret);
    }

    #[cfg(feature = "filesystem")]
    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    #[tokio::test]
    async fn encrypt_decrypt_file_transfer_remote(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
        #[values(SecurityLevel::Standard, SecurityLevel::Reinforced)] security_level: SecurityLevel,
    ) {
        citadel_logging::setup_log();
        let (plaintext, bytes_ret, sa_alice, sa_bob) = test_file_transfer_inner(
            TransferType::RemoteEncryptedVirtualFilesystem {
                virtual_path: PathBuf::from("/"),
                security_level,
            },
            enx,
            kem,
            sig,
            security_level,
        )
        .await;
        assert_ne!(plaintext, bytes_ret);
        let decrypted = sa_alice.local_decrypt(&bytes_ret, security_level).unwrap();
        assert_eq!(decrypted, plaintext);
        assert!(sa_bob.local_decrypt(&bytes_ret, security_level).is_err());
    }

    async fn test_file_transfer_inner(
        transfer_type: TransferType,
        enx: EncryptionAlgorithm,
        kem: KemAlgorithm,
        sig: SigAlgorithm,
        security_level: SecurityLevel,
    ) -> (&'static [u8], Vec<u8>, StackedRatchet, StackedRatchet) {
        use citadel_crypt::scramble::crypt_splitter::GroupReceiverStatus;
        use std::time::Instant;
        use tokio::sync::mpsc::channel;

        use citadel_crypt::streaming_crypt_scrambler::scramble_encrypt_source;

        let (alice, bob) = gen::<StackedRatchet>(0, 0, security_level, enx + kem + sig);
        let (pseudo_static_aux_ratchet_alice, pseudo_static_aux_ratchet_bob) =
            gen::<StackedRatchet>(0, 0, security_level, enx + kem + sig);

        let cmp = include_bytes!("../../resources/TheBridge.pdf");
        let source = PathBuf::from("../resources/TheBridge.pdf");
        let (group_sender_tx, mut group_sender_rx) = channel(1);
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel();
        let (bytes, _num_groups, _mxbpg) = scramble_encrypt_source::<_, _, HEADER_LEN>(
            source,
            None,
            99,
            group_sender_tx,
            stop_rx,
            security_level,
            alice.clone(),
            pseudo_static_aux_ratchet_alice.clone(),
            HEADER_LEN,
            bob.get_cid(),
            0,
            transfer_type,
            header_inscribe,
        )
        .unwrap();

        let mut _i: usize = 0;
        let now = Instant::now();
        let mut bytes_ret = Vec::new();
        let _compressed_len: usize = 0;
        let _decompressed_len: usize = 0;

        while let Some(gs) = group_sender_rx.recv().await {
            let mut gs = gs.unwrap();
            let config = gs.get_receiver_config();
            log::error!(target: "citadel", "Config: {:?}", config);
            let mut receiver = GroupReceiver::new(config.clone(), 0, 0);
            let group_id = config.group_id;
            let mut _seq = 0;
            let _now = Instant::now();
            'here: while let Some(mut packet) = gs.get_next_packet() {
                let packet_payload = packet.packet.split_off(HEADER_LEN);
                let result = receiver.on_packet_received(
                    group_id as u64,
                    packet.vector.true_sequence,
                    packet.vector.wave_id,
                    &bob,
                    packet_payload,
                );
                //dbg!(&result);
                if let GroupReceiverStatus::GROUP_COMPLETE(_group_id) = result {
                    bytes_ret.extend_from_slice(receiver.finalize().as_slice());
                    break 'here;
                }
                //seq += 1;
            }
            //i += 1;
        }

        let delta = now.elapsed();
        let megabytes = bytes as f32 / 1_000_000f32;
        let mbs = megabytes / delta.as_secs_f32();
        println!(
            "Done receiving all. {} time, {} bytes. {} Mb/s",
            delta.as_millis(),
            bytes,
            mbs
        );
        (
            cmp,
            bytes_ret,
            pseudo_static_aux_ratchet_alice,
            pseudo_static_aux_ratchet_bob,
        )
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn test_drill_encrypt_decrypt_basic(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        citadel_logging::setup_log();
        test_harness(enx + kem + sig, |alice, bob, _, data| {
            let encrypted = alice.encrypt(data).unwrap();
            let decrypted = bob.decrypt(encrypted).unwrap();
            assert_eq!(decrypted, data);
        });
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn test_drill_encrypt_decrypt_scrambler(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        citadel_logging::setup_log();
        test_harness(enx + kem + sig, |alice, bob, _, data| {
            let encrypted = alice.encrypt_scrambler(data).unwrap();
            let decrypted = bob.decrypt_scrambler(encrypted).unwrap();
            assert_eq!(decrypted, data);
        });
    }

    #[rstest]
    #[case(
        EncryptionAlgorithm::AES_GCM_256,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Ascon80pq,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::ChaCha20Poly_1305,
        KemAlgorithm::Kyber,
        SigAlgorithm::None
    )]
    #[case(
        EncryptionAlgorithm::Kyber,
        KemAlgorithm::Kyber,
        SigAlgorithm::Falcon1024
    )]
    fn test_drill_local_encrypt_decrypt(
        #[case] enx: EncryptionAlgorithm,
        #[case] kem: KemAlgorithm,
        #[case] sig: SigAlgorithm,
    ) {
        citadel_logging::setup_log();
        test_harness(enx + kem + sig, |alice, bob, sec, data| {
            let encrypted = alice.local_encrypt(data, sec).unwrap();
            assert!(bob.local_decrypt(&*encrypted, sec).is_err());
            let decrypted = alice.local_decrypt(encrypted, sec).unwrap();
            assert_eq!(decrypted, data);
        });
    }

    fn test_harness(
        params: CryptoParameters,
        fx: impl Fn(&StackedRatchet, &StackedRatchet, SecurityLevel, &[u8]),
    ) {
        let data = Vec::from("Hello, world!");

        for sec in 0..5 {
            let security_level = SecurityLevel::from(sec);
            let (hr_alice, hr_bob) = gen::<StackedRatchet>(0, 0, security_level, params);
            for idx in 0..data.len() {
                (fx)(&hr_alice, &hr_bob, security_level, &data[..idx]);
            }
        }
    }
}
