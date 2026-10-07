use crate::parser::RawEvent;

pub struct SanitizationPipeline;

impl SanitizationPipeline {
    pub fn sanitize_event(mut event: RawEvent) -> RawEvent {
        // 纯契约空桩
        event
    }
}
