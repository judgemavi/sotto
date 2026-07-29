#[cfg(test)]
mod tests {
    use asr::spsc_ring;

    #[test]
    fn audio_callback_push_path_uses_fixed_storage() {
        let (mut producer, mut consumer) = spsc_ring(4);
        producer.push_slice(&[0.1, 0.2, 0.3, 0.4, 0.5]);
        let (samples, skipped) = consumer.take_latest_for_test(4);
        assert_eq!(samples, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(skipped, 0);
    }
}
