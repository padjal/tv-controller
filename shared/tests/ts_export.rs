#[test]
fn export_types() {
    use shared::*;
    Device::export_all().unwrap();
    Video::export_all().unwrap();
    PlayCommand::export_all().unwrap();
    PlaybackRequest::export_all().unwrap();
    AgentStatus::export_all().unwrap();
    SseEvent::export_all().unwrap();
}
