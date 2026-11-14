//! A Supermicro BMC's web session: its own pages' login, which the KVM's token lives and dies with, and the CGI its
//! pages read what Redfish doesn't say from, and change what Redfish can't.

use std::sync::atomic::Ordering::Relaxed;

use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};

use super::redfish::Connection;
use super::{ok, transport};
use crate::{Error, GOODBYE, Result};

/// A logged-in web session, by its cookie.
pub struct Web {
    http: reqwest::Client,
    url: String,
    sid: String,
}

impl Web {
    /// Logs in as the login page does, but in plain text where its script sends base64: the X10 takes it, as it does
    /// from the other native viewers. No session means a wrong password, which we don't try again.
    pub async fn login(connection: &Connection) -> Result<Self> {
        if connection.refused.load(Relaxed) {
            return Err(Error::Unauthorized);
        }
        let form = form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &connection.user)
            .append_pair("pwd", &connection.password)
            .finish();
        let request = connection.http.post(format!("{}/cgi/login.cgi", connection.url));
        let request = request.header(CONTENT_TYPE, "application/x-www-form-urlencoded").body(form);
        let response =
            ok(request.send().await.map_err(|error| connection.pin.mismatch().unwrap_or_else(|| transport(error)))?)?;
        // Some firmware clears the cookie before it sets it.
        let cookies = response.headers().get_all(SET_COOKIE).iter().filter_map(|cookie| cookie.to_str().ok());
        let sid =
            cookies.filter_map(|cookie| cookie.strip_prefix("SID=")?.split(';').next()).rfind(|sid| !sid.is_empty());
        let Some(sid) = sid else {
            connection.refused.store(true, Relaxed);
            return Err(Error::Unauthorized);
        };
        Ok(Self { http: connection.http.clone(), url: connection.url.clone(), sid: sid.to_owned() })
    }

    /// Logs in, does `what` in the session and logs out again: the BMC has only a few, which stay until they time out.
    pub async fn with<T>(connection: &Connection, what: impl AsyncFnOnce(&Self) -> Result<T>) -> Result<T> {
        let web = Self::login(connection).await?;
        let result = what(&web).await;
        web.logout().await;
        result
    }

    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// The KVM token in the viewer's page, the same for every load in one web session.
    pub async fn token(&self) -> Result<String> {
        let page = self.page("man_ikvm_html5_bootstrap").await?;
        between(&page, r#"id="entry_value" value=""#, "\"")
            .filter(|token| !token.is_empty())
            .ok_or_else(|| Error::Protocol("the BMC's viewer page holds no KVM token".into()))
    }

    /// What the pages' CGI says to each of `reads` (`GENERIC_INFO.XML`), XML, with the token against cross-site
    /// requests its pages carry. Only reads: their names end in `.XML` and none starts with `SET_`.
    pub async fn read<const N: usize>(&self, reads: [&str; N]) -> Result<[String; N]> {
        let token = self.token_against_forgery().await?;
        let mut answers = [const { String::new() }; N];
        for (answer, read) in answers.iter_mut().zip(reads) {
            debug_assert!(read.ends_with(".XML") && !read.starts_with("SET_"));
            *answer = self.post("/cgi/ipmi.cgi", &token, format!("{read}=(0,0)")).await?;
        }
        Ok(answers)
    }

    /// Makes the change its pages' `op` makes (`config_port`), with `fields` as they send them, and says the answer.
    pub async fn change(&self, op: &str, fields: &[(String, String)]) -> Result<String> {
        let token = self.token_against_forgery().await?;
        let form = form_urlencoded::Serializer::new(String::new()).append_pair("op", op).extend_pairs(fields).finish();
        self.post("/cgi/op.cgi", &token, form).await
    }

    /// The token against cross-site requests its pages carry, which its CGI wants with each request.
    async fn token_against_forgery(&self) -> Result<String> {
        let page = self.page("sys_info").await?;
        Ok(between(&page, r#"SmcCsrfInsert ("CSRF_TOKEN", ""#, "\"").unwrap_or_default())
    }

    async fn post(&self, path: &str, token: &str, body: String) -> Result<String> {
        let request = self.http.post(format!("{}{path}", self.url)).header(COOKIE, self.cookie());
        let response = request.header("CSRF_TOKEN", token).header(CONTENT_TYPE, "application/x-www-form-urlencoded");
        ok(response.body(body).send().await.map_err(transport)?)?.text().await.map_err(transport)
    }

    /// One of its pages, by the name its menu gives it.
    async fn page(&self, name: &str) -> Result<String> {
        let page = self.get(&format!("/cgi/url_redirect.cgi?url_name={name}")).send().await;
        ok(page.map_err(transport)?)?.text().await.map_err(transport)
    }

    /// Ends the web session, and with it the token, within `GOODBYE`.
    pub async fn logout(&self) {
        let _ = tokio::time::timeout(GOODBYE, self.get("/cgi/logout.cgi").send()).await;
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{path}", self.url)).header(COOKIE, self.cookie())
    }

    fn cookie(&self) -> String {
        format!("SID={}", self.sid)
    }
}

/// What's in `text` between `before` and the next `after`.
fn between(text: &str, before: &str, after: &str) -> Option<String> {
    Some(text.split_once(before)?.1.split_once(after)?.0.to_owned())
}
