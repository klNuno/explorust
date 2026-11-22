use crate::model::Group;

pub struct Pin<T> {
    pub source: Option<usize>,
    pub key: String,
    pub item: Option<T>,
}

pub enum Focus {
    Start,
    App { key: String, source: Option<usize> },
}

impl Focus {
    pub fn at<T>(slot: usize, groups: &[Group], pins: &[Pin<T>]) -> Self {
        if slot == 0 {
            return Self::Start;
        }
        let group = &groups[slot - 1];
        Self::App {
            key: group.key.clone(),
            source: group.pin.and_then(|p| pins[p].source),
        }
    }

    pub fn slot<T>(&self, groups: &[Group], pins: &[Pin<T>]) -> Option<usize> {
        let Self::App { key, source } = self else {
            return Some(0);
        };
        source
            .and_then(|source| {
                groups
                    .iter()
                    .position(|g| g.pin.is_some_and(|p| pins[p].source == Some(source)))
            })
            .or_else(|| groups.iter().position(|g| g.key == *key))
            .map(|i| i + 1)
    }
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
    use crate::model::Model;

    #[test]
    fn removing_a_placeholder_keeps_focus_on_the_same_running_app() {
        let mut pins: Vec<Pin<()>> = placeholders(1);
        let mut model = Model::default();
        model.upsert(10, "notes");
        model.upsert(11, "calculator");
        model.pins = pins.iter().map(|pin| pin.key.clone()).collect();
        let focus = Focus::at(2, &model.groups(), &pins);

        assert!(resolve(&mut pins, 0, None));
        model.pins.clear();
        assert_eq!(focus.slot(&model.groups(), &pins), Some(1));
        model.remove(10);
        assert_eq!(focus.slot(&model.groups(), &pins), None);
    }

    #[test]
    fn a_placeholder_keeps_focus_when_it_learns_its_identity() {
        let mut pins = placeholders(2);
        let mut model = Model::default();
        model.pins = pins.iter().map(|pin| pin.key.clone()).collect();
        let focus = Focus::at(2, &model.groups(), &pins);

        assert!(resolve(&mut pins, 0, None));
        assert!(resolve(&mut pins, 1, Some(("calculator".into(), ()))));
        model.pins = pins.iter().map(|pin| pin.key.clone()).collect();
        assert_eq!(focus.slot(&model.groups(), &pins), Some(1));
    }

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
