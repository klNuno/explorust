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

#[derive(Clone, Copy)]
pub enum Read {
    Shortcut,
    Identity,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadUpdate {
    Publish,
    Merge,
    Remove,
    Skip,
}

#[derive(Default)]
pub struct Reads {
    shortcut: Option<bool>,
    identity: Option<bool>,
}

impl Reads {
    pub fn complete(&mut self, read: Read, found: bool) -> ReadUpdate {
        let (current, other) = match read {
            Read::Shortcut => (&mut self.shortcut, self.identity),
            Read::Identity => (&mut self.identity, self.shortcut),
        };
        *current = Some(found);
        match (read, found, other) {
            (Read::Shortcut, true, Some(true)) => ReadUpdate::Merge,
            (_, true, _) => ReadUpdate::Publish,
            (_, false, Some(false)) => ReadUpdate::Remove,
            (_, false, _) => ReadUpdate::Skip,
        }
    }
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

    #[test]
    fn a_pin_can_launch_when_only_the_shortcut_is_readable() {
        let mut reads = Reads::default();
        assert_eq!(reads.complete(Read::Identity, false), ReadUpdate::Skip);
        assert_eq!(reads.complete(Read::Shortcut, true), ReadUpdate::Publish);

        let mut reads = Reads::default();
        assert_eq!(reads.complete(Read::Shortcut, true), ReadUpdate::Publish);
        assert_eq!(reads.complete(Read::Identity, false), ReadUpdate::Skip);
    }

    #[test]
    fn a_late_shortcut_does_not_replace_the_resolved_identity() {
        let mut reads = Reads::default();
        assert_eq!(reads.complete(Read::Identity, true), ReadUpdate::Publish);
        assert_eq!(reads.complete(Read::Shortcut, true), ReadUpdate::Merge);

        let mut reads = Reads::default();
        assert_eq!(reads.complete(Read::Shortcut, true), ReadUpdate::Publish);
        assert_eq!(reads.complete(Read::Identity, true), ReadUpdate::Publish);
    }

    #[test]
    fn a_pin_is_removed_only_when_both_reads_fail() {
        for (first, second) in [
            (Read::Shortcut, Read::Identity),
            (Read::Identity, Read::Shortcut),
        ] {
            let mut reads = Reads::default();
            assert_eq!(reads.complete(first, false), ReadUpdate::Skip);
            assert_eq!(reads.complete(second, false), ReadUpdate::Remove);
        }
    }
}
