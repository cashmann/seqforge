//! The engine's intrusive circular lists (`AudioListItem`), as an arena.
//!
//! Note allocation and stealing depend on list order (pop from the back, push
//! to the front, scan from the head), so this mirrors the C exactly, including
//! `remove` not decrementing the head's count.

pub type NodeId = usize;
pub type PoolId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListValue {
    None,
    Note(usize),
    Layer(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// `None` = detached (the C code's `prev == NULL`).
    pub prev: Option<NodeId>,
    pub next: NodeId,
    pub value: ListValue,
    pub count: i32,
    pub pool: Option<PoolId>,
}

/// `NotePool`: four list heads.
#[derive(Clone, Copy, Debug)]
pub struct NotePool {
    pub disabled: NodeId,
    pub decaying: NodeId,
    pub releasing: NodeId,
    pub active: NodeId,
}

#[derive(Default)]
pub struct Lists {
    pub nodes: Vec<Node>,
    pub pools: Vec<NotePool>,
}

impl Lists {
    /// A detached item node.
    pub fn new_item(&mut self, value: ListValue) -> NodeId {
        self.nodes.push(Node {
            prev: None,
            next: 0,
            value,
            count: 0,
            pool: None,
        });
        self.nodes.len() - 1
    }

    /// An empty list head (points at itself).
    pub fn new_head(&mut self, pool: Option<PoolId>) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            prev: Some(id),
            next: id,
            value: ListValue::None,
            count: 0,
            pool,
        });
        id
    }

    pub fn new_pool(&mut self) -> PoolId {
        let pool = self.pools.len();
        let disabled = self.new_head(Some(pool));
        let decaying = self.new_head(Some(pool));
        let releasing = self.new_head(Some(pool));
        let active = self.new_head(Some(pool));
        self.pools.push(NotePool {
            disabled,
            decaying,
            releasing,
            active,
        });
        pool
    }

    /// `Audio_InitNoteList`: reset a head to empty.
    pub fn reset_head(&mut self, head: NodeId) {
        let n = &mut self.nodes[head];
        n.prev = Some(head);
        n.next = head;
        n.count = 0;
    }

    pub fn reset_pool(&mut self, pool: PoolId) {
        let p = self.pools[pool];
        for h in [p.disabled, p.decaying, p.releasing, p.active] {
            self.reset_head(h);
        }
    }

    /// `AudioSeq_AudioListPushBack`
    pub fn push_back(&mut self, list: NodeId, item: NodeId) {
        if self.nodes[item].prev.is_none() {
            let last = self.nodes[list].prev.unwrap_or(list);
            self.nodes[last].next = item;
            self.nodes[item].prev = Some(last);
            self.nodes[item].next = list;
            self.nodes[list].prev = Some(item);
            self.nodes[list].count += 1;
            self.nodes[item].pool = self.nodes[list].pool;
        }
    }

    /// `AudioSeq_AudioListPopBack`
    pub fn pop_back(&mut self, list: NodeId) -> Option<ListValue> {
        let item = self.nodes[list].prev.unwrap_or(list);
        if item == list {
            return None;
        }
        let before = self.nodes[item].prev.unwrap_or(list);
        self.nodes[before].next = list;
        self.nodes[list].prev = Some(before);
        self.nodes[item].prev = None;
        self.nodes[list].count -= 1;
        Some(self.nodes[item].value)
    }

    /// `Audio_AudioListPushFront`
    pub fn push_front(&mut self, list: NodeId, item: NodeId) {
        if self.nodes[item].prev.is_none() {
            let first = self.nodes[list].next;
            self.nodes[item].prev = Some(list);
            self.nodes[item].next = first;
            self.nodes[first].prev = Some(item);
            self.nodes[list].next = item;
            self.nodes[list].count += 1;
            self.nodes[item].pool = self.nodes[list].pool;
        }
    }

    /// `Audio_AudioListRemove`
    pub fn remove(&mut self, item: NodeId) {
        if let Some(prev) = self.nodes[item].prev {
            let next = self.nodes[item].next;
            self.nodes[prev].next = next;
            self.nodes[next].prev = Some(prev);
            self.nodes[item].prev = None;
        }
    }

    /// The pool an item last joined (`item->pool`).
    pub fn pool_of(&self, item: NodeId) -> Option<PoolId> {
        self.nodes[item].pool
    }

    /// Items of a list, head to tail.
    pub fn iter(&self, list: NodeId) -> impl Iterator<Item = ListValue> + '_ {
        let mut cur = self.nodes[list].next;
        std::iter::from_fn(move || {
            if cur == list {
                return None;
            }
            let v = self.nodes[cur].value;
            cur = self.nodes[cur].next;
            Some(v)
        })
    }

    pub fn first(&self, list: NodeId) -> Option<NodeId> {
        let n = self.nodes[list].next;
        (n != list).then_some(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_pop_order() {
        let mut l = Lists::default();
        let head = l.new_head(None);
        let a = l.new_item(ListValue::Note(0));
        let b = l.new_item(ListValue::Note(1));
        let c = l.new_item(ListValue::Note(2));
        l.push_back(head, a);
        l.push_back(head, b);
        l.push_front(head, c);
        assert_eq!(
            l.iter(head).collect::<Vec<_>>(),
            [ListValue::Note(2), ListValue::Note(0), ListValue::Note(1)]
        );
        assert_eq!(l.pop_back(head), Some(ListValue::Note(1)));
        l.remove(c);
        assert_eq!(l.iter(head).collect::<Vec<_>>(), [ListValue::Note(0)]);
        // Pushing an attached item is a no-op.
        l.push_back(head, a);
        assert_eq!(l.iter(head).count(), 1);
    }
}
