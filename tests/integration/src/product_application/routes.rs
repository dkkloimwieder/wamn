use anyhow::Context as _;
use serde_json::{Value, json};

pub struct Routes {
    pub endpoint: String,
    pub host: String,
    pub bearer: String,
    pub client: reqwest::Client,
}

impl Routes {
    /// Post one `widget.create` request element, and return its answer request element.
    pub async fn create(
        &self,
        request_id: &str,
        key: &str,
        id: &str,
        code: &str,
    ) -> anyhow::Result<Value> {
        self.post(
            "/widget/create",
            json!({"request_id": request_id, "id": id, "idempotency_key": key, "code": code}),
        )
        .await
    }

    /// Post one `widget.record_batch` request element with `lines` of `[widget_id,
    /// amount]`, and return its answer request element.
    pub async fn record_batch(
        &self,
        request_id: &str,
        key: &str,
        lines: &[(&str, &str)],
    ) -> anyhow::Result<Value> {
        let line = lines
            .iter()
            .map(|(widget_id, amount)| json!({"widget_id": widget_id, "amount": amount}))
            .collect::<Vec<_>>();
        self.post(
            "/widget/record_batch",
            json!({"request_id": request_id, "value": {
                "idempotency_key": key,
                "note": null,
                "maker_id": null,
                "expected_edit_version": "1",
                "grade": "first",
                "inspector_id": null,
                "line": line,
            }}),
        )
        .await
    }

    /// Post one request element to `path`, and return its answer request element
    /// without its request id.
    pub async fn post(&self, path: &str, request_element: Value) -> anyhow::Result<Value> {
        let request_id = request_element["request_id"].clone();
        let response = self
            .client
            .post(format!("{}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .json(&json!([request_element]))
            .send()
            .await?;
        let status = response.status().as_u16();
        let text = response.text().await?;
        anyhow::ensure!(status == 200, "{path} answered {status}: {text}");
        let answer: Value = serde_json::from_str(&text)?;
        let mut request_element = answer[0].clone();
        anyhow::ensure!(
            request_element["request_id"] == request_id,
            "the answer keeps the request id: {answer}"
        );
        request_element
            .as_object_mut()
            .context("an answer request element is an object")?
            .remove("request_id");
        Ok(request_element)
    }
}

impl std::fmt::Debug for Routes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Routes")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}
