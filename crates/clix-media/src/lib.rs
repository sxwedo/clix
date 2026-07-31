use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clix_core::{
    fs::{atomic_write, parent_or_current},
    ui,
};
use reqwest::header::HeaderMap;

const MAX_CONCURRENT_REQUESTS: usize = 4;
const MAX_MEDIA_BYTES: usize = 32 * 1024 * 1024;

struct MediaJob {
    request_index: usize,
    url: String,
    destination: PathBuf,
    relative_path: String,
    headers: HeaderMap,
}

#[derive(Debug, Clone)]
pub struct MediaRequest {
    url: String,
    file_name: String,
    headers: HeaderMap,
}

impl MediaRequest {
    #[must_use]
    pub fn new(url: impl Into<String>, file_name: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            file_name: file_name.into(),
            headers: HeaderMap::new(),
        }
    }

    #[must_use]
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableMedia {
    pub request_index: usize,
    pub relative_path: String,
}

/// Download a batch of media files into the output file's sibling `media/` directory.
///
/// Individual request failures are reported to the terminal and do not fail the batch.
///
/// # Errors
///
/// Returns an error when the media directory cannot be created.
pub async fn download_media(
    client: &reqwest::Client,
    output_path: &Path,
    progress_label: &str,
    requests: Vec<MediaRequest>,
) -> Result<Vec<AvailableMedia>> {
    let media_dir = parent_or_current(output_path).join("media");
    fs::create_dir_all(&media_dir).context("failed to create media directory")?;

    let spinner = ui::create_spinner(progress_label);
    let mut downloaded = 0;
    let mut reused = 0;
    let mut failed = 0;
    let mut available = Vec::new();
    let mut jobs = Vec::new();

    for (request_index, request) in requests.into_iter().enumerate() {
        let destination = media_dir.join(&request.file_name);
        let relative_path = format!("./media/{}", request.file_name);
        if destination.exists() {
            reused += 1;
            available.push(AvailableMedia {
                request_index,
                relative_path,
            });
        } else {
            jobs.push(MediaJob {
                request_index,
                url: request.url,
                destination,
                relative_path,
                headers: request.headers,
            });
        }
    }

    let job_count = jobs.len();
    let mut pending = jobs.into_iter();
    let mut tasks = tokio::task::JoinSet::new();
    for job in pending.by_ref().take(MAX_CONCURRENT_REQUESTS) {
        spawn_download(&mut tasks, client.clone(), job);
    }

    let mut completed = 0;
    while let Some(joined) = tasks.join_next().await {
        completed += 1;
        spinner.set_message(format!("{progress_label} ({completed}/{job_count})"));
        match joined {
            Ok((job, Ok(()))) => {
                downloaded += 1;
                available.push(AvailableMedia {
                    request_index: job.request_index,
                    relative_path: job.relative_path,
                });
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
            spawn_download(&mut tasks, client.clone(), job);
        }
    }

    available.sort_unstable_by_key(|media| media.request_index);
    spinner.finish_and_clear();
    ui::success(format!(
        "media: {downloaded} downloaded, {reused} reused, {failed} failed; directory {}",
        ui::style_bold(&media_dir.display().to_string())
    ));

    Ok(available)
}

fn spawn_download(
    tasks: &mut tokio::task::JoinSet<(MediaJob, Result<()>)>,
    client: reqwest::Client,
    job: MediaJob,
) {
    tasks.spawn(async move {
        let result = download_one(&client, &job).await;
        (job, result)
    });
}

async fn download_one(client: &reqwest::Client, job: &MediaJob) -> Result<()> {
    let mut response = client
        .get(&job.url)
        .headers(job.headers.clone())
        .send()
        .await
        .with_context(|| format!("failed to request media {}", job.url))?
        .error_for_status()
        .with_context(|| format!("received error response for media {}", job.url))?;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_MEDIA_BYTES as u64)
    {
        bail!("media exceeds the {MAX_MEDIA_BYTES}-byte download limit");
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_MEDIA_BYTES),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .with_context(|| format!("failed to download bytes for media {}", job.url))?
    {
        if chunk.len() > MAX_MEDIA_BYTES.saturating_sub(bytes.len()) {
            bail!("media exceeds the {MAX_MEDIA_BYTES}-byte download limit");
        }
        bytes.extend_from_slice(&chunk);
    }

    let destination = job.destination.clone();
    tokio::task::spawn_blocking(move || atomic_write(&destination, &bytes))
        .await
        .context("media persistence task failed")?
        .with_context(|| format!("failed to write media {}", job.destination.display()))
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _, net::TcpListener, thread};

    use reqwest::header::{HeaderValue, REFERER};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, path},
    };

    use super::*;

    #[tokio::test]
    async fn downloads_once_then_reuses_the_existing_media_file() {
        let server = MockServer::start().await;
        Mock::given(path("/image"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"image-data"))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let output_path = directory.path().join("article.md");
        let client = reqwest::Client::new();
        let request = || MediaRequest::new(format!("{}/image", server.uri()), "post_1.jpg");

        let first = download_media(
            &client,
            &output_path,
            "downloading test image",
            vec![request()],
        )
        .await
        .expect("first download should succeed");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].relative_path, "./media/post_1.jpg");
        assert_eq!(
            fs::read(directory.path().join("media/post_1.jpg"))
                .expect("downloaded media should be readable"),
            b"image-data"
        );

        let second = download_media(&client, &output_path, "reusing test image", vec![request()])
            .await
            .expect("second download should reuse the file");
        assert_eq!(second, first);
    }

    #[tokio::test]
    async fn oversized_media_is_isolated_without_leaving_a_file() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
        let address = listener
            .local_addr()
            .expect("test server should have an address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .expect("test server should accept a request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_MEDIA_BYTES + 1
            )
            .expect("test server should write a response");
        });
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let output_path = directory.path().join("article.md");

        let available = download_media(
            &reqwest::Client::new(),
            &output_path,
            "rejecting oversized image",
            vec![MediaRequest::new(
                format!("http://{address}/oversized"),
                "too-large.jpg",
            )],
        )
        .await
        .expect("one failed request should not fail the batch");

        assert!(available.is_empty());
        assert!(!directory.path().join("media/too-large.jpg").exists());
        server.join().expect("test server should stop cleanly");
    }

    #[tokio::test]
    async fn request_specific_headers_are_forwarded() {
        let server = MockServer::start().await;
        Mock::given(path("/protected"))
            .and(header("referer", "https://example.com/"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"protected-image"))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let mut headers = HeaderMap::new();
        headers.insert(REFERER, HeaderValue::from_static("https://example.com/"));

        let available = download_media(
            &reqwest::Client::new(),
            &directory.path().join("article.md"),
            "downloading protected image",
            vec![
                MediaRequest::new(format!("{}/protected", server.uri()), "protected.jpg")
                    .with_headers(headers),
            ],
        )
        .await
        .expect("request headers should be forwarded");

        assert_eq!(available.len(), 1);
        assert_eq!(available[0].relative_path, "./media/protected.jpg");
    }
}
