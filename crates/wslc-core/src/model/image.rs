//! 镜像模型。

use serde::{Deserialize, Serialize};

use super::parse_size;

/// `wslc images --format json` 的一行。
///
/// ⚠️ **同一个镜像会出现多行**：不同 `Repository` 引用指向同一个 `ID`
/// （实机采集到 `docker.1panel.live/library/hello-world` 与 `hello-world`
/// 都是 `e2ac70e7319a`）。UI 若按镜像去重，应使用 `ID`；
/// 若按"可引用的名字"列表，则应使用 `Reference`。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ImageListItem {
    /// 12 位镜像 ID。
    #[serde(rename = "ID", default)]
    pub id: String,
    /// 仓库名，可能是 `<none>`。
    #[serde(rename = "Repository", default)]
    pub repository: String,
    /// 标签，可能是 `<none>`。
    #[serde(rename = "Tag", default)]
    pub tag: String,
    /// 摘要，通常是 `<none>`。
    #[serde(rename = "Digest", default)]
    pub digest: String,
    /// 绝对时间。
    #[serde(rename = "CreatedAt", default)]
    pub created_at: String,
    /// 相对时间。
    #[serde(rename = "CreatedSince", default)]
    pub created_since: String,
    /// `8.42MB`
    #[serde(rename = "Size", default)]
    pub size: String,
    /// 常为 `N/A`。
    #[serde(rename = "SharedSize", default)]
    pub shared_size: String,
    /// 常为 `N/A`。
    #[serde(rename = "UniqueSize", default)]
    pub unique_size: String,
    /// 使用该镜像的容器数（**数字字符串**）。
    #[serde(rename = "Containers", default)]
    pub containers: String,
}

impl ImageListItem {
    /// `Repository:Tag`，`<none>` 时回退到短 ID。
    pub fn reference(&self) -> String {
        let repo_is_none = self.repository.is_empty() || self.repository == "<none>";
        if repo_is_none {
            return self.short_id().to_owned();
        }
        let tag_is_none = self.tag.is_empty() || self.tag == "<none>";
        if tag_is_none {
            self.repository.clone()
        } else {
            format!("{}:{}", self.repository, self.tag)
        }
    }

    /// 12 位短 ID。
    pub fn short_id(&self) -> &str {
        let n = self.id.len().min(12);
        &self.id[..n]
    }

    /// 是否为悬空镜像（没有仓库名也没有标签）。
    pub fn is_dangling(&self) -> bool {
        (self.repository.is_empty() || self.repository == "<none>")
            && (self.tag.is_empty() || self.tag == "<none>")
    }

    /// 体积（字节）。
    pub fn size_bytes(&self) -> Option<f64> {
        parse_size(&self.size)
    }

    /// 引用该镜像的容器数。
    pub fn container_count(&self) -> u32 {
        self.containers.trim().parse().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/images.jsonl");

    #[test]
    fn parses_real_images_output() {
        let items: Vec<ImageListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "320994c3b997");
        assert_eq!(items[0].repository, "docker.1ms.run/library/alpine");
        assert_eq!(items[0].tag, "latest");
        assert_eq!(items[0].digest, "<none>");
        assert_eq!(items[0].size, "8.42MB");
        assert_eq!(items[0].reference(), "docker.1ms.run/library/alpine:latest");
        assert_eq!(items[0].container_count(), 0);
        assert!(!items[0].is_dangling());
    }

    #[test]
    fn same_image_appears_under_multiple_references() {
        let items: Vec<ImageListItem> = crate::jsonl::parse_lines(FIXTURE).unwrap();
        let hello: Vec<_> = items.iter().filter(|i| i.id == "e2ac70e7319a").collect();
        assert_eq!(hello.len(), 2, "同一镜像 ID 应出现两次（不同 Repository）");
        // 注意：`hello` 是 `Vec<&ImageListItem>`，所以 `hello.iter()` 给出的是
        // `&&ImageListItem`，不能直接把 `ImageListItem::reference` 当函数用。
        let refs: Vec<String> = hello.iter().map(|i| i.reference()).collect();
        assert!(refs.contains(&"docker.1panel.live/library/hello-world:latest".to_owned()));
        assert!(refs.contains(&"hello-world:latest".to_owned()));
    }

    #[test]
    fn dangling_detection() {
        let img = ImageListItem {
            id: "abc123def456".into(),
            repository: "<none>".into(),
            tag: "<none>".into(),
            ..Default::default()
        };
        assert!(img.is_dangling());
        assert_eq!(img.reference(), "abc123def456");
    }

    #[test]
    fn size_parsing() {
        let img = ImageListItem {
            size: "10.1kB".into(),
            ..Default::default()
        };
        let bytes = img.size_bytes().expect("应能解析");
        assert!((bytes - 10_100.0).abs() < 1e-6, "实际 {bytes}");
    }

    #[test]
    fn missing_fields_do_not_panic() {
        let img: ImageListItem = serde_json::from_str("{}").unwrap();
        assert_eq!(img.reference(), "");
        assert_eq!(img.container_count(), 0);
        assert_eq!(img.size_bytes(), None);
    }
}
