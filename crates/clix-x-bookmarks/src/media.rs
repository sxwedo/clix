use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clix_core::{
    fs::{atomic_write, parent_or_current},
    ui,
};
use clix_x_api::media_extension;

use crate::model::TweetBookmark;

const MAX_CONCURRENT_MEDIA_REQUESTS: usize = 4;

struct MediaJob {
    bookmark_index: usize,
    media_index: usize,
    url: String,
    destination: PathBuf,
    relative_path: String,
}

pub async fn download_all_media(
    client: &reqwest::Client,
    bookmarks: &mut [TweetBookmark],
    output_path: &Path,
) -> Result<()> {
    let base_dir = parent_or_current(output_path);
    let media_dir = base_dir.join("media");
    fs::create_dir_all(&media_dir).context("failed to create media directory")?;

    let spinner = ui::create_spinner("downloading attached media images...");
    let mut downloaded = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut available_media = Vec::new();
    let mut jobs = Vec::new();

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
            let destination = media_dir.join(&file_name);
            let relative_path = format!("./media/{file_name}");

            if destination.exists() {
                skipped += 1;
                available_media.push((bookmark_index, media_index, relative_path));
            } else {
                jobs.push(MediaJob {
                    bookmark_index,
                    media_index,
                    url: media_url.clone(),
                    destination,
                    relative_path,
                });
            }
        }
    }

    let job_count = jobs.len();
    let mut pending = jobs.into_iter();
    let mut tasks = tokio::task::JoinSet::new();
    for job in pending.by_ref().take(MAX_CONCURRENT_MEDIA_REQUESTS) {
        spawn_media_download(&mut tasks, client.clone(), job);
    }

    let mut completed = 0;
    while let Some(joined) = tasks.join_next().await {
        completed += 1;
        spinner.set_message(format!(
            "downloading attached media images ({completed}/{job_count})"
        ));
        match joined {
            Ok((job, Ok(()))) => {
                downloaded += 1;
                available_media.push((job.bookmark_index, job.media_index, job.relative_path));
            }
            Ok((job, Err(error))) => {
                failed += 1;
                ui::warn(format!("could not download {}: {error:#}", job.url));
            }
            Err(error) => {
                failed += 1;
                ui::warn(format!("media download task failed: {error}"));
            }
        }
        if let Some(job) = pending.next() {
            spawn_media_download(&mut tasks, client.clone(), job);
        }
    }

    available_media
        .sort_unstable_by_key(|(bookmark_index, media_index, _)| (*bookmark_index, *media_index));
    for (bookmark_index, _, relative_path) in available_media {
        let local_media = &mut bookmarks[bookmark_index].local_media;
        if !local_media.contains(&relative_path) {
            local_media.push(relative_path);
        }
    }

    spinner.finish_and_clear();
    ui::success(format!(
        "media: {downloaded} downloaded, {skipped} reused, {failed} failed; directory {}",
        ui::style_bold(&media_dir.display().to_string())
    ));
    Ok(())
}

fn spawn_media_download(
    tasks: &mut tokio::task::JoinSet<(MediaJob, Result<()>)>,
    client: reqwest::Client,
    job: MediaJob,
) {
    tasks.spawn(async move {
        let result = async {
            let response = client.get(&job.url).send().await?.error_for_status()?;
            let bytes = response.bytes().await?;
            let destination = job.destination.clone();
            tokio::task::spawn_blocking(move || atomic_write(&destination, &bytes))
                .await
                .context("media persistence task failed")?
        }
        .await;
        (job, result)
    });
}
