//! Displays attached to the desktop, in physical pixels of the virtual desktop.

/// A display as the host knows it and as the protocol carries it. Its `id` is the same for the
/// same monitor across restarts and re-plugging (Windows: its `QueryDisplayConfig` device path,
/// hashed; macOS: the display UUID).
pub use wire::types::Display as DisplayInfo;

/// Left to right, then top to bottom.
pub fn sort(list: &mut [DisplayInfo]) {
    list.sort_by_key(|d| (d.x, d.y));
}

/// The display to use: the wanted one while it is attached, otherwise the primary one.
pub fn choose<'a>(list: &'a [DisplayInfo], want: Option<&str>) -> Option<&'a DisplayInfo> {
    want.and_then(|id| list.iter().find(|d| d.id == id)).or_else(|| list.iter().find(|d| d.primary)).or(list.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(id: &str, x: i32, primary: bool) -> DisplayInfo {
        DisplayInfo { id: id.into(), x, y: 0, w: 100, h: 100, primary }
    }

    #[test]
    fn choose_falls_back_to_primary() {
        let mut list = vec![d("B", 100, false), d("A", 0, true), d("C", -100, false)];
        sort(&mut list);
        assert_eq!(list.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["C", "A", "B"]);
        assert_eq!(choose(&list, Some("B")).unwrap().id, "B");
        assert_eq!(choose(&list, Some("gone")).unwrap().id, "A");
        assert_eq!(choose(&list, None).unwrap().id, "A");
        assert!(choose(&[], None).is_none());
    }
}
