use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, RETRY_AFTER};
use serde_json::Value;

const API_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RETRIES: u32 = 3;

/// Read-only client for the (unofficial) Ed API.
#[derive(Clone)]
pub struct EdClient {
    http: Client,
    base: String,
    token: String,
}

#[derive(Debug, Clone, Default)]
pub struct Course {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub year: String,
    pub session: String,
    pub status: String,
    pub role: String,
}

#[derive(Debug, Clone, Default)]
pub struct Module {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Lesson {
    pub id: i64,
    pub course_id: i64,
    pub module_id: i64,
    pub number: i64,
    pub index: i64,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub locked: bool,
    pub slide_count: i64,
    pub outline: String,
    pub available_at: String,
    pub due_at: String,
    pub slides: Vec<Slide>,
}

#[derive(Debug, Clone, Default)]
pub struct Slide {
    pub id: i64,
    pub index: i64,
    pub title: String,
    pub kind: String,
    pub content: String,
    pub file_url: String,
    pub is_hidden: bool,
}

pub struct Me {
    pub name: String,
    pub courses: Vec<Course>,
}

impl EdClient {
    pub fn new(base: &str, token: &str) -> Result<Self> {
        let http = Client::builder()
            .timeout(API_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("edstem-tui/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let base = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{base}/")
        };
        Ok(Self {
            http,
            base,
            token: token.to_string(),
        })
    }

    /// The signed-in user and their enrolled courses (active courses first).
    pub fn user(&self) -> Result<Me> {
        let data = self.get("user")?;
        let mut courses: Vec<Course> = arr(&data, "courses")
            .iter()
            .map(|enrollment| {
                let course = enrollment.get("course").unwrap_or(enrollment);
                Course {
                    id: int(course, "id"),
                    code: string(course, "code"),
                    name: string(course, "name"),
                    year: string(course, "year"),
                    session: string(course, "session"),
                    status: string(course, "status"),
                    role: enrollment
                        .get("role")
                        .map(|role| string(role, "role"))
                        .unwrap_or_default(),
                }
            })
            .filter(|course| course.id > 0)
            .collect();
        courses.sort_by_key(|course| course.status == "archived");
        let name = data
            .get("user")
            .map(|user| string(user, "name"))
            .unwrap_or_default();
        Ok(Me { name, courses })
    }

    pub fn lessons(&self, course_id: i64) -> Result<(Vec<Module>, Vec<Lesson>)> {
        let data = self.get(&format!("courses/{course_id}/lessons"))?;
        let modules = arr(&data, "modules")
            .iter()
            .map(|module| Module {
                id: int(module, "id"),
                name: string(module, "name"),
            })
            .collect();
        let mut lessons: Vec<Lesson> = arr(&data, "lessons").iter().map(parse_lesson).collect();
        lessons.sort_by_key(|lesson| lesson.index);
        Ok((modules, lessons))
    }

    pub fn lesson(&self, lesson_id: i64) -> Result<Lesson> {
        let data = self.get(&format!("lessons/{lesson_id}"))?;
        Ok(parse_lesson(data.get("lesson").unwrap_or(&data)))
    }

    pub fn slide(&self, slide_id: i64) -> Result<Slide> {
        let data = self.get(&format!("lessons/slides/{slide_id}"))?;
        Ok(parse_slide(data.get("slide").unwrap_or(&data)))
    }

    /// GET with retries on 429/5xx. Only reads are ever sent, so retrying is safe.
    fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let mut attempt = 0;
        loop {
            let resp = self
                .http
                .get(&url)
                .bearer_auth(&self.token)
                .header(ACCEPT, "application/json")
                .send()
                .context("failed to reach the Ed API")?;
            let status = resp.status();

            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
                let wait = resp
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .map(Duration::from_secs)
                    .unwrap_or(Duration::from_secs(1 << attempt));
                thread::sleep(wait.min(Duration::from_secs(30)));
                attempt += 1;
                continue;
            }

            if status.is_redirection() {
                bail!(
                    "Ed API redirected (HTTP {}); check the region or EDSTEM_BASE_URL",
                    status.as_u16()
                );
            }

            let body = resp.text().context("failed to read the Ed API response")?;
            let payload: Option<Value> = serde_json::from_str(&body).ok();
            if !status.is_success() {
                let code = payload.as_ref().map(|p| string(p, "code")).unwrap_or_default();
                let message = payload
                    .as_ref()
                    .map(|p| string(p, "message"))
                    .unwrap_or_default();
                if status.as_u16() == 401 || code == "bad_token" {
                    bail!(
                        "authentication failed (HTTP {}): check your Ed API token",
                        status.as_u16()
                    );
                }
                if message.is_empty() {
                    bail!("Ed API error (HTTP {})", status.as_u16());
                }
                bail!("Ed API error (HTTP {}): {message}", status.as_u16());
            }
            return payload.context("Ed API returned a non-JSON response");
        }
    }
}

fn parse_lesson(v: &Value) -> Lesson {
    let available_at = first_non_empty(&[
        string(v, "effective_available_at"),
        string(v, "available_at"),
    ]);
    Lesson {
        id: int(v, "id"),
        course_id: int(v, "course_id"),
        module_id: int(v, "module_id"),
        number: int(v, "number"),
        index: int(v, "index"),
        title: string(v, "title"),
        kind: string(v, "type"),
        status: string(v, "status"),
        // `openable` is not an access flag: Ed sends `false` even for active lessons the
        // student has completed. Only a release date still in the future locks a lesson;
        // a missing or unparsable date must not hide content.
        locked: is_future(&available_at),
        slide_count: int(v, "slide_count"),
        outline: string(v, "outline"),
        available_at,
        due_at: first_non_empty(&[string(v, "effective_due_at"), string(v, "due_at")]),
        slides: {
            let mut slides: Vec<Slide> = arr(v, "slides").iter().map(parse_slide).collect();
            slides.sort_by_key(|slide| slide.index);
            slides
        },
    }
}

fn parse_slide(v: &Value) -> Slide {
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    Slide {
        id: int(v, "id"),
        index: int(v, "index"),
        title: string(v, "title"),
        kind: string(v, "type"),
        content: first_non_empty(&[
            string(v, "content"),
            string(v, "passage"),
            string(&data, "content"),
            string(&data, "passage"),
        ]),
        file_url: string(v, "file_url"),
        is_hidden: v.get("is_hidden") == Some(&Value::Bool(true)),
    }
}

fn is_future(timestamp: &str) -> bool {
    DateTime::parse_from_rfc3339(timestamp.trim()).is_ok_and(|at| at > Utc::now())
}

fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .find(|value| !value.trim().is_empty())
        .cloned()
        .unwrap_or_default()
}

fn string(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn int(v: &Value, key: &str) -> i64 {
    match v.get(key) {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lesson_parsing_is_lenient() {
        let lesson = parse_lesson(&json!({
            "id": "12", "module_id": null, "title": "Week 1",
            "slides": [
                {"id": 2, "index": 2, "type": "pdf", "file_url": "https://static.us.edusercontent.com/files/a"},
                {"id": 1, "index": 1, "type": "document", "data": {"content": "<document/>"}}
            ]
        }));
        assert_eq!(lesson.id, 12);
        assert_eq!(lesson.module_id, 0);
        assert_eq!(lesson.slides[0].id, 1);
        assert_eq!(lesson.slides[0].content, "<document/>");
        assert_eq!(lesson.slides[1].kind, "pdf");
    }

    #[test]
    fn openable_false_does_not_lock() {
        // Ed sends this for active lessons a student has already completed.
        let lesson = parse_lesson(&json!({
            "id": 1, "openable": false, "state": "active", "status": "completed",
            "available_at": null, "effective_available_at": null
        }));
        assert!(!lesson.locked);
    }

    #[test]
    fn future_release_locks() {
        let lesson = parse_lesson(&json!({
            "id": 1, "available_at": "2000-01-01T00:00:00+11:00",
            "effective_available_at": "2999-01-01T00:00:00.123456+11:00"
        }));
        assert!(lesson.locked);
        assert!(!parse_lesson(&json!({"id": 1, "available_at": "2000-01-01T00:00:00Z"})).locked);
        assert!(!parse_lesson(&json!({"id": 1, "available_at": "not a date"})).locked);
    }
}
