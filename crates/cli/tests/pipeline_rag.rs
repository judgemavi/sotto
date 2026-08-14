#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

mod tests {
    #[test]
    fn file_ingestion_uses_the_generic_resource_kind() {
        assert_eq!(
            cli::pipeline::FILE_INGEST_KIND,
            rag::DocumentKind::ResourceDocument
        );
        assert_eq!(
            cli::pipeline::FILE_INGEST_KIND.as_str(),
            "resource_document"
        );
    }
}
