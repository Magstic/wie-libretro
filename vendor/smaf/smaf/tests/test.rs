use smaf::{
    BaseBit, Channel, FormatType, PCMAudioSequenceData, PCMAudioSequenceEvent, PCMAudioTrackChunk, PCMDataChunk, PcmWaveFormat, ScoreTrackChunk,
    ScoreTrackSequenceEvent, SequenceData, Smaf, SmafChunk, StreamWaveFormat,
};

#[test]
fn test_bell_load() -> anyhow::Result<()> {
    let data = include_bytes!("../../test_data/bell.mmf");
    let file = Smaf::parse(data)?;

    assert_eq!(file.chunks.len(), 3);
    assert!(matches!(file.chunks[0], SmafChunk::ContentsInfo(_)));
    assert!(matches!(file.chunks[1], SmafChunk::OptionalData(_)));
    assert!(matches!(file.chunks[2], SmafChunk::ScoreTrack(6, _)));

    if let SmafChunk::ScoreTrack(_, x) = &file.chunks[2] {
        assert_eq!(x.format_type, FormatType::MobileStandardNoCompress);

        assert_eq!(x.chunks.len(), 3);
        assert!(matches!(x.chunks[0], ScoreTrackChunk::SetupData(_)));
        assert!(matches!(x.chunks[1], ScoreTrackChunk::SequenceData(_)));
        assert!(matches!(x.chunks[2], ScoreTrackChunk::PCMData(_)));

        if let ScoreTrackChunk::PCMData(x) = &x.chunks[2] {
            assert_eq!(x.len(), 1);
            assert!(matches!(x[0], PCMDataChunk::WaveData(1, _)));

            let smaf::PCMDataChunk::WaveData(_, x) = &x[0];

            assert_eq!(x.channel, Channel::Mono);
            assert_eq!(x.format, StreamWaveFormat::YamahaADPCM);
            assert_eq!(x.base_bit, BaseBit::Bit4);
            assert_eq!(x.sampling_freq, 22050);

            assert_eq!(x.wave_data.len(), 367616);
        } else {
            panic!("Expected PcmData chunk");
        }
    } else {
        panic!("Expected ScoreTrack chunk");
    }

    Ok(())
}

#[test]
fn test_wave_load() -> anyhow::Result<()> {
    let data = include_bytes!("../../test_data/wave.mmf");
    let file = Smaf::parse(data)?;

    assert_eq!(file.chunks.len(), 2);
    assert!(matches!(file.chunks[0], SmafChunk::ContentsInfo(_)));
    assert!(matches!(file.chunks[1], SmafChunk::PCMAudioTrack(0, _)));

    if let SmafChunk::PCMAudioTrack(_, x) = &file.chunks[1] {
        assert_eq!(x.format_type, 0);
        assert_eq!(x.sequence_type, 0);
        assert_eq!(x.channel, Channel::Mono);
        assert_eq!(x.format, PcmWaveFormat::Adpcm);
        assert_eq!(x.sampling_freq, 8000);
        assert_eq!(x.base_bit, BaseBit::Bit4);
        assert_eq!(x.timebase_d, 4);
        assert_eq!(x.timebase_g, 4);

        assert_eq!(x.chunks.len(), 3);

        assert!(matches!(x.chunks[0], PCMAudioTrackChunk::SeekAndPhraseInfo(_)));
        assert!(matches!(x.chunks[1], PCMAudioTrackChunk::SequenceData(_)));
        assert!(matches!(x.chunks[2], PCMAudioTrackChunk::WaveData(1, _)));
    } else {
        panic!("Expected PCMAudioTrack chunk");
    }

    Ok(())
}

#[test]
fn test_midi_load() -> anyhow::Result<()> {
    let data = include_bytes!("../../test_data/midi.mmf");
    let file = Smaf::parse(data)?;

    assert_eq!(file.chunks.len(), 3);
    assert!(matches!(file.chunks[0], SmafChunk::ContentsInfo(_)));
    assert!(matches!(file.chunks[1], SmafChunk::OptionalData(_)));
    assert!(matches!(file.chunks[2], SmafChunk::ScoreTrack(5, _)));

    if let SmafChunk::ScoreTrack(_, x) = &file.chunks[2] {
        assert_eq!(x.format_type, FormatType::MobileStandardNoCompress);

        assert_eq!(x.chunks.len(), 2);
        assert!(matches!(x.chunks[0], ScoreTrackChunk::SetupData(_)));
        assert!(matches!(x.chunks[1], ScoreTrackChunk::SequenceData(_)));
    } else {
        panic!("Expected ScoreTrack chunk");
    }

    Ok(())
}

#[test]
fn test_compressed_score_track_header_loads() -> anyhow::Result<()> {
    let mut mtr = vec![0x01, 0x00, 0x02, 0x02];
    mtr.extend([0; 16]);

    let file = wrap_smaf_chunk(b"MTR0", &mtr);
    let file = Smaf::parse(&file)?;

    assert!(matches!(file.chunks[0], SmafChunk::ScoreTrack(_, _)));
    if let SmafChunk::ScoreTrack(_, track) = &file.chunks[0] {
        assert_eq!(track.format_type, FormatType::MobileStandardCompress);
        assert_eq!(track.channel_status.len(), 16);
    }

    Ok(())
}

#[test]
fn test_unknown_score_track_subchunk_is_skipped() -> anyhow::Result<()> {
    let mut mtr = vec![0x02, 0x00, 0x02, 0x02];
    mtr.extend([0; 16]);
    mtr.extend(b"ZZZZ");
    mtr.extend(1u32.to_be_bytes());
    mtr.push(0x7f);

    let file = wrap_smaf_chunk(b"MTR0", &mtr);
    let file = Smaf::parse(&file)?;

    if let SmafChunk::ScoreTrack(_, track) = &file.chunks[0] {
        if let ScoreTrackChunk::Unknown(tag, data) = &track.chunks[0] {
            assert_eq!(*tag, b"ZZZZ");
            assert_eq!(*data, [0x7f]);
        } else {
            panic!("Expected unknown score track subchunk");
        }
    } else {
        panic!("Expected ScoreTrack chunk");
    }

    Ok(())
}

#[test]
fn test_pcm_long_expression_consumes_value_byte() -> anyhow::Result<()> {
    let (_, events) = PCMAudioSequenceData::parse(&[0x00, 0x00, 0x36, 0x55, 0x00, 0x00, 0x00, 0x00]).map_err(|e| anyhow::anyhow!("{e:?}"))?;

    assert!(matches!(events[0].event, PCMAudioSequenceEvent::Expression { channel: 0, value: 0x55 }));

    Ok(())
}

#[test]
fn test_mobile_variable_number_uses_seven_payload_bits() -> anyhow::Result<()> {
    let (_, events) = SequenceData::parse_mobile(&[0xc0, 0x00, 0x80, 0x3c, 0x01, 0x00, 0xff, 0x2f, 0x00])
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    assert_eq!(events[0].duration, 8192);
    assert!(matches!(
        events[0].event,
        ScoreTrackSequenceEvent::NoteMessage {
            note: 0x3c,
            velocity: None,
            ..
        }
    ));

    Ok(())
}

#[test]
fn test_handy_variable_number_matches_smaf2midi_converter() -> anyhow::Result<()> {
    let (_, events) = SequenceData::parse_handy(&[0xc0, 0x00, 0x11, 0x01, 0x00, 0x00, 0x00, 0x00])
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    assert_eq!(events[0].duration, 8320);

    Ok(())
}

#[test]
fn test_handy_short_pitch_bend_matches_smaf2midi_converter() -> anyhow::Result<()> {
    let (_, events) = SequenceData::parse_handy(&[0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x00])
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    assert!(matches!(
        events[0].event,
        ScoreTrackSequenceEvent::PitchBend {
            channel: 0,
            value: 1024
        }
    ));

    Ok(())
}

#[test]
fn test_handy_note_keeps_raw_pitch_for_player_mapping() -> anyhow::Result<()> {
    let (_, events) = SequenceData::parse_handy(&[0x00, 0x29, 0x01, 0x00, 0x00, 0x00, 0x00])
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    assert!(matches!(
        events[0].event,
        ScoreTrackSequenceEvent::NoteMessage {
            channel: 0,
            note: 33,
            velocity: None,
            gate_time: 1
        }
    ));

    Ok(())
}

#[test]
fn test_mobile_reserved_status_is_ignored() -> anyhow::Result<()> {
    let (_, events) = SequenceData::parse_mobile(&[0x00, 0xa0, 0x01, 0x02, 0x00, 0xff, 0x2f, 0x00])
        .map_err(|e| anyhow::anyhow!("{e:?}"))?;

    assert!(matches!(events[0].event, ScoreTrackSequenceEvent::Nop));

    Ok(())
}

fn wrap_smaf_chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend(b"MMMD");
    file.extend(((8 + data.len() + 2) as u32).to_be_bytes());
    file.extend(tag);
    file.extend((data.len() as u32).to_be_bytes());
    file.extend(data);
    file.extend([0, 0]);
    file
}
