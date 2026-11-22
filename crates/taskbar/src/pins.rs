pub struct Pin<T> {
    pub source: Option<usize>,
    pub key: String,
    pub item: Option<T>,
}

pub fn placeholders<T>(count: usize) -> Vec<Pin<T>> {
    (0..count)
        .map(|slot| Pin {
            source: Some(slot),
            // Neither an AUMID nor a path can contain a NUL.
            key: format!("\0pin{slot}"),
            item: None,
        })
        .collect()
}

pub fn resolve<T>(pins: &mut Vec<Pin<T>>, slot: usize, item: Option<(String, T)>) -> bool {
    let Some(index) = pins.iter().position(|pin| pin.source == Some(slot)) else {
        return false;
    };
    if let Some((key, item)) = item {
        pins[index].key = key;
        pins[index].item = Some(item);
    } else {
        pins.remove(index);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_slots_fill_in_registry_order_despite_out_of_order_results() {
        let mut pins = placeholders(3);
        assert!(resolve(&mut pins, 2, Some(("last".into(), 3))));
        assert!(resolve(&mut pins, 0, Some(("first".into(), 1))));
        assert!(resolve(&mut pins, 1, None));
        assert!(!resolve(&mut pins, 1, Some(("stale".into(), 2))));
        assert_eq!(
            pins.iter().map(|pin| pin.key.as_str()).collect::<Vec<_>>(),
            ["first", "last"]
        );
        assert_eq!(
            pins.iter().map(|pin| pin.item.unwrap()).collect::<Vec<_>>(),
            [1, 3]
        );
    }
}
