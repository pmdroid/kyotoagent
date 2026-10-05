use std::io::Cursor;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use image::{ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};

pub const MAX_IMAGES: usize = 4;
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachment {
    pub name: String,
    pub mime_type: String,
    pub data: String,
}

impl ImageAttachment {
    pub fn from_file_bytes(name: &str, bytes: &[u8]) -> Result<Self, String> {
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|error| error.to_string())?;
        let mut limits = Limits::default();
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|error| error.to_string())?
            .thumbnail(1024, 1024);
        let mut encoded = Cursor::new(Vec::new());
        if image.color().has_alpha() {
            image::DynamicImage::ImageRgba8(image.to_rgba8())
                .write_to(&mut encoded, ImageFormat::Png)
                .map_err(|error| error.to_string())?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 75)
                .encode_image(&image.to_rgb8())
                .map_err(|error| error.to_string())?;
        }
        Self::from_bytes(name, encoded.get_ref())
    }

    pub fn from_bytes(name: &str, bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("Images must be 5 MiB or smaller.".into());
        }
        let format =
            image::guess_format(bytes).map_err(|_| "Choose a PNG, JPEG, or WebP image.")?;
        let mime_type = match format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::WebP => "image/webp",
            _ => return Err("Choose a PNG, JPEG, or WebP image.".into()),
        };
        let attachment = Self {
            name: name.to_string(),
            mime_type: mime_type.to_string(),
            data: STANDARD.encode(bytes),
        };
        attachment.validate()?;
        Ok(attachment)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 255 || self.name.chars().any(char::is_control)
        {
            return Err("Image names must contain between 1 and 255 bytes.".into());
        }
        if self.data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("Images must be 5 MiB or smaller.".into());
        }
        let bytes = STANDARD
            .decode(&self.data)
            .map_err(|_| "Invalid image data.")?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err("Images must be 5 MiB or smaller.".into());
        }
        let format = match self.mime_type.as_str() {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            "image/webp" => ImageFormat::WebP,
            _ => return Err("Choose a PNG, JPEG, or WebP image.".into()),
        };
        if image::guess_format(&bytes).ok() != Some(format) {
            return Err("Image type does not match its data.".into());
        }
        let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
        let mut limits = Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        reader
            .decode()
            .map_err(|_| "Image could not be decoded within the size limit.")?;
        Ok(())
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.data)
    }
}

pub fn validate_images(images: &[ImageAttachment]) -> Result<(), String> {
    if images.len() > MAX_IMAGES {
        return Err("Attach at most four images per message.".into());
    }
    for image in images {
        image.validate()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_images_round_trip_and_invalid_uploads_are_rejected() {
        let image = ImageAttachment::from_bytes("dog.png", crate::splash::PNG).unwrap();
        assert!(image.data_url().starts_with("data:image/png;base64,"));
        assert_eq!(STANDARD.decode(&image.data).unwrap(), crate::splash::PNG);
        validate_images(std::slice::from_ref(&image)).unwrap();
        assert!(validate_images(&vec![image.clone(); MAX_IMAGES + 1]).is_err());
        let mut invalid = image.clone();
        invalid.mime_type = "image/jpeg".into();
        assert!(invalid.validate().is_err());
        invalid = image.clone();
        invalid.data = STANDARD.encode(b"not an image");
        assert!(invalid.validate().is_err());
        invalid = image;
        invalid.data = "A".repeat(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4);
        assert!(invalid.validate().is_err());
    }
}
