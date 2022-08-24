extern crate infer;
pub mod ape;
pub mod flac;
pub mod id3;
pub mod mp4;

pub mod format;
pub mod map;
pub mod picture;

use super::util::{dedup, take_first};
use core::convert::AsRef;
use eyre::{Result, WrapErr};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter, Result as FormatResult};
use std::fs::copy;
use std::path::{Path, PathBuf};

use format::Format;
use picture::Picture;

use self::map::TagKey;

#[derive(Clone, Debug)]
pub struct TrackFile {
    path: PathBuf,
    format: Format,
    tag: Box<dyn Tag>,
}

impl TrackFile {
    pub fn open(path: &PathBuf) -> Result<TrackFile> {
        let format = Format::from_path(path)
            .wrap_err(format!("Could not identify format for file: {:?}", path))?;
        let tag = match format {
            Format::FLAC => flac::Tag::from_path(path),
            Format::MP4 => mp4::Tag::from_path(path),
            Format::ID3 => id3::Tag::from_path(path),
            Format::APE => ape::Tag::from_path(path),
        }
        .wrap_err(format!("Could not read metadata from file: {:?}", path))?;
        Ok(TrackFile {
            path: path.to_path_buf(),
            format,
            tag,
        })
    }

    fn r#move(&mut self, path: &PathBuf) -> Result<PathBuf> {
        copy(&self.path, path)?;
        let path = self.path.clone();
        self.path = path.to_path_buf();
        Ok(path)
    }

    fn write(&mut self) -> Result<()> {
        self.tag
            .write_to_path(&self.path)
            .wrap_err(format!("Could not write tags to file: {:?}", self.path))
    }
}

pub trait TagFrom {
    fn from_path<P>(path: P) -> Result<Box<dyn Tag>>
    where
        P: AsRef<Path>;
}

pub trait TagClone {
    fn clone_box(&self) -> Box<dyn Tag>;
}

impl<T> TagClone for T
where
    T: 'static + Tag + Clone,
{
    fn clone_box(&self) -> Box<dyn Tag> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Tag> {
    fn clone(&self) -> Box<dyn Tag> {
        self.clone_box()
    }
}

impl Debug for Box<dyn Tag> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FormatResult {
        let mut str = f.debug_struct("Tag");
        for (k, v) in self.get_all() {
            str.field(&k, &v);
        }
        str.field("pictures", &self.get_pictures());
        str.finish()
    }
}

pub trait Tag: TagClone {
    fn get_str(&self, key: &str) -> Option<Vec<String>>;
    fn set_str(&mut self, key: &str, values: Vec<String>) -> Result<()>;
    fn get_all(&self) -> HashMap<String, Vec<String>>;
    fn get_pictures(&self) -> Result<Vec<Picture>>;
    fn set_pictures(&mut self, pictures: Vec<Picture>) -> Result<()>;

    fn str_to_key(&self, str: &str) -> Option<TagKey>;
    fn key_to_str(&self, key: TagKey) -> Option<&'static str>;

    fn write_to_path(&mut self, path: &PathBuf) -> Result<()>;
}

pub trait TrackLike {
    fn artists(&self) -> Result<Vec<String>>;
    fn album_artists(&self) -> Result<Vec<String>>;
    fn title(&self) -> Result<String>;
    fn album_title(&self) -> Result<String>;
    fn length(&self) -> Result<u64>;
}

impl TrackLike for TrackFile {
    fn artists(&self) -> Result<Vec<String>> {
        Ok(dedup(self.get_tag(TagKey::AlbumArtist)?))
    }
    fn album_artists(&self) -> Result<Vec<String>> {
        self.get_tag(TagKey::AlbumArtist)
    }
    fn title(&self) -> Result<String> {
        take_first(self.get_tag(TagKey::TrackTitle)?, "Track has no title")
    }
    fn album_title(&self) -> Result<String> {
        take_first(self.get_tag(TagKey::Album)?, "Track has no album title")
    }
    fn length(&self) -> Result<u64> {
        unimplemented!();
    }
}
