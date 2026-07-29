#[cfg(test)]
mod tests {
    use std::time::Duration;

    use prosody::{Annotator, Config, Event, select};
    use sotto_core::{Annotation, Source, Utterance};

    fn utterance(source: Source, start_ms: u64, end_ms: u64, text: &str) -> Utterance {
        Utterance {
            source,
            start: Duration::from_millis(start_ms),
            end: Duration::from_millis(end_ms),
            text: text.into(),
            avg_logprob: -0.1,
            annotations: Vec::new(),
        }
    }

    #[test]
    fn distinguishes_pause_and_interruption() {
        let mut value = Annotator::default();
        assert!(
            value
                .observe(Event::Utterance(&utterance(
                    Source::System,
                    0,
                    1_000,
                    "hello"
                )))
                .is_empty()
        );
        assert_eq!(
            value.observe(Event::Utterance(&utterance(
                Source::Mic,
                1_800,
                2_200,
                "yes"
            ))),
            vec![Annotation::Pause(Duration::from_millis(800))]
        );
        assert_eq!(
            value.observe(Event::Utterance(&utterance(
                Source::System,
                2_000,
                3_000,
                "but"
            ))),
            vec![Annotation::Interruption { by: Source::System }]
        );
    }

    #[test]
    fn simultaneous_starts_are_not_arbitrarily_interruptions() {
        let mut value = Annotator::default();
        value.observe(Event::Utterance(&utterance(Source::Mic, 0, 1_000, "hello")));
        assert!(
            !value
                .observe(Event::Utterance(&utterance(
                    Source::System,
                    0,
                    1_000,
                    "different"
                )))
                .iter()
                .any(|item| matches!(item, Annotation::Interruption { .. }))
        );
    }

    #[test]
    fn matching_overlap_is_echo_leakage() {
        let mut value = Annotator::default();
        value.observe(Event::Utterance(&utterance(
            Source::System,
            0,
            2_000,
            "pricing is flexible",
        )));
        assert!(
            value
                .observe(Event::Utterance(&utterance(
                    Source::Mic,
                    100,
                    1_900,
                    "pricing is flexible"
                )))
                .is_empty()
        );
    }

    #[test]
    fn drift_offsets_change_cross_stream_ordering() {
        let mut value = Annotator::new(Config {
            stream_offsets: [0.3, 0.0],
            ..Config::default()
        });
        value.observe(Event::Utterance(&utterance(
            Source::System,
            0,
            1_000,
            "customer",
        )));
        assert!(
            value
                .observe(Event::Utterance(&utterance(Source::Mic, 800, 1_200, "rep")))
                .is_empty()
        );
    }

    #[test]
    fn speech_rate_uses_adaptive_per_speaker_baseline() {
        let mut value = Annotator::default();
        value.observe(Event::Utterance(&utterance(
            Source::System,
            0,
            10_000,
            "one two three four five six seven eight nine ten",
        )));
        let annotations = value.observe(Event::Utterance(&utterance(
            Source::System,
            10_000,
            12_000,
            "one two three four five six seven eight nine ten",
        )));
        assert!(
            annotations
                .iter()
                .any(|item| matches!(item, Annotation::SpeechRate(rate) if *rate > 250.0))
        );
        assert_eq!(
            value.last_delta().map(|delta| delta.talk_time_ratio),
            Some(1.0)
        );
    }

    #[test]
    fn talk_time_is_queryable() {
        let mut value = Annotator::default();
        value.observe(Event::Utterance(&utterance(Source::Mic, 0, 3_000, "rep")));
        value.observe(Event::Utterance(&utterance(
            Source::System,
            3_000,
            4_000,
            "customer",
        )));
        assert_eq!(value.recent_talk_time_ratio(Source::Mic), 0.75);
        assert_eq!(value.recent_talk_time_ratio(Source::System), 0.25);
    }

    #[test]
    fn hesitancy_is_under_eager() {
        let mut value = Annotator::default();
        assert!(
            value
                .observe(Event::Utterance(&utterance(
                    Source::System,
                    0,
                    1_000,
                    "um okay"
                )))
                .is_empty()
        );
        assert!(
            value
                .observe(Event::Utterance(&utterance(
                    Source::System,
                    1_000,
                    2_000,
                    "um uh maybe"
                )))
                .contains(&Annotation::Hesitant)
        );
    }

    #[test]
    fn selection_drops_low_salience_first_and_core_renders() {
        let candidates = vec![
            Annotation::TalkTimeRatio(0.4),
            Annotation::Hesitant,
            Annotation::Pause(Duration::from_millis(2_500)),
        ];
        let chosen = select(&candidates, 4);
        assert_eq!(
            chosen,
            vec![
                Annotation::Hesitant,
                Annotation::Pause(Duration::from_millis(2_500))
            ]
        );
        let mut value = utterance(Source::System, 0, 1_000, "sure, sounds fine");
        value.annotations = chosen;
        assert_eq!(
            value.render_inline(),
            "[customer, hesitant, 2.5s pause] \"sure, sounds fine\""
        );
    }
}
