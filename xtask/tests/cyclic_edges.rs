use std::collections::BTreeSet;
use xtask::coupling::{Edge, cyclic_edges, feedback_edges};

fn graph(mask: usize) -> BTreeSet<Edge> {
    (0..9)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| (format!("n{}", bit / 3), format!("n{}", bit % 3)))
        .collect()
}

fn transitive_closure(mask: usize) -> [[bool; 3]; 3] {
    let mut reachable = [[false; 3]; 3];
    for bit in 0..9 {
        reachable[bit / 3][bit % 3] = mask & (1 << bit) != 0;
    }
    for via in 0..3 {
        for from in 0..3 {
            for to in 0..3 {
                reachable[from][to] |= reachable[from][via] && reachable[via][to];
            }
        }
    }
    reachable
}

#[test]
fn complete_inventory_matches_all_three_node_graphs_including_self_loops() {
    for mask in 0..512 {
        let edges = graph(mask);
        let reachable = transitive_closure(mask);
        let expected = (0..9)
            .filter(|bit| mask & (1 << bit) != 0 && reachable[bit % 3][bit / 3])
            .map(|bit| (format!("n{}", bit / 3), format!("n{}", bit % 3)))
            .collect();
        let actual = cyclic_edges(&edges);
        assert_eq!(actual, expected, "graph mask {mask}");
        assert!(feedback_edges(&edges).is_subset(&actual));
    }
}
