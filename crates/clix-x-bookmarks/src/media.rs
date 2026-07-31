use std::path::Path;

use anyhow::Result;
use clix_media::{MediaRequest, download_media};
use clix_x_api::media_extension;

use crate::model::TweetBookmark;

pub async fn download_all_media(
    client: &reqwest::Client,
    bookmarks: &mut [TweetBookmark],
    output_path: &Path,
) -> Result<()> {
    let mut owners = Vec::new();
    let mut requests = Vec::new();

    for (bookmark_index, bookmark) in bookmarks.iter().enumerate() {
        for (media_index, media_url) in bookmark.media.iter().enumerate() {
            let ext = media_extension(media_url);
            let file_name = format!(
                "{}_{}_{}.{}",
                bookmark.author_handle,
                bookmark.id,
                media_index + 1,
                ext
            );
            owners.push(bookmark_index);
            requests.push(MediaRequest::new(media_url, file_name));
        }
    }

    let available_media = download_media(
        client,
        output_path,
        "downloading attached media images...",
        requests,
    )
    .await?;
    for available in available_media {
        let bookmark_index = owners[available.request_index];
        let local_media = &mut bookmarks[bookmark_index].local_media;
        if !local_media.contains(&available.relative_path) {
            local_media.push(available.relative_path);
        }
    }
    Ok(())
}
