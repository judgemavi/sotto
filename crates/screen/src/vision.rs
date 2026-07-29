use objc2::{
    AnyThread,
    rc::{Retained, autoreleasepool},
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSError, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::{Frame, OcrEngine, ScreenError, rgba_pixels};

/// Apple Vision text recognition for sampled frames.
///
/// Vision requests are serialized because `VNRequest` instances are stateful. Call this
/// from the screen worker, never from ScreenCaptureKit's frame callback.
pub struct VisionOcr;

impl VisionOcr {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for VisionOcr {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrEngine for VisionOcr {
    fn recognize(&self, frame: &Frame) -> Result<String, ScreenError> {
        autoreleasepool(|_| recognize_inner(frame))
    }
}

fn recognize_inner(frame: &Frame) -> Result<String, ScreenError> {
    let png = encode_png(frame)?;
    let data = NSData::with_bytes(&png);
    let options = NSDictionary::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &options,
    );
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    let erased: Retained<VNRequest> = request.clone().into_super().into_super();
    let requests = NSArray::from_retained_slice(&[erased]);
    handler
        .performRequests_error(&requests)
        .map_err(format_vision_error)?;
    let observations = request.results().unwrap_or_default();
    let mut lines = Vec::new();
    for observation in observations.iter() {
        let observation: &VNRecognizedTextObservation = &observation;
        if let Some(candidate) = observation.topCandidates(1).firstObject() {
            lines.push(candidate.string().to_string());
        }
    }
    Ok(lines.join("\n"))
}

fn encode_png(frame: &Frame) -> Result<Vec<u8>, ScreenError> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba_pixels(frame))?;
        writer.finish()?;
    }
    Ok(bytes)
}

fn format_vision_error(error: Retained<NSError>) -> ScreenError {
    let description: Retained<NSString> = error.localizedDescription();
    ScreenError::Ocr(description.to_string())
}
