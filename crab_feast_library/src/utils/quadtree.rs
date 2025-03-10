

use bevy::{math::bounding::{Aabb2d, BoundingVolume, IntersectsVolume}, prelude::*};
use std::{ptr, sync::Arc};

pub trait QuadTreeData {
    fn aabb_2d(&self) -> Aabb2d;
}

pub struct QuadNode<T> {
    boundary: Aabb2d,
    children: Option<[Box<QuadNode<T>>; 4]>,
    data: Vec<Arc<T>>,
}

impl <T : QuadTreeData> QuadNode<T> {
    pub fn new(boundary: Aabb2d) -> Self {
        Self {
            children: None,
            boundary,
            data: Vec::new(),
        }
    }

    pub fn insert(&mut self, data: Arc<T>) {
        if self.children.is_none() && self.data.len() > 1 && self.boundary.half_size().xy().length_squared() > 6.0 {
            self.subdivide();
        }

        match self.children {
            Some(ref mut children) => {
                for child in children {
                    if child.boundary.contains(&data.aabb_2d()) {
                        child.insert(data);
                        break;
                    }
                }
            }
            None => {
                self.data.push(data);
            }
        }
    }

    pub fn remove(&mut self, data: &T) {
        for i in 0..self.data.len() {
            if ptr::eq(self.data[i].as_ref(), data) {
                self.data.remove(i);
                return;
            }
        }

        if let Some(children) = &mut self.children {
            for child in children {
                if child.boundary.contains(&data.aabb_2d()) {
                    child.remove(data);
                    break;
                }
            }
        }
    }

    pub fn query(&self, boundary: &Aabb2d) -> Vec<Arc<T>> {
        let mut results = Vec::new();
        if !self.boundary.intersects(boundary) {
            return results;
        }

        for data in &self.data {
            if boundary.intersects(&data.aabb_2d()) {
                results.push(data.clone());
            }
        }

        if let Some(children) = &self.children {
            for child in children {
                results.extend(child.query(boundary));
            }
        }
        results
    }
    
    pub fn iter_children(&self) -> Option<impl Iterator<Item = &Box<QuadNode<T>>> + '_> {
        self.children.as_ref().map(|c| c.iter())
    }

    pub fn iter_data(&self) -> impl Iterator<Item = &Arc<T>> + '_ {
        self.data.iter()
    }

    pub fn left_top(&self) -> Option<&Box<QuadNode<T>>> {
        if let Some(children) = &self.children {
            return Some(&children[0]);
        }
        None
    }

    pub fn right_top(&self) -> Option<&Box<QuadNode<T>>> {
        if let Some(children) = &self.children {
            return Some(&children[1]);
        }
        None
    }

    pub fn left_bottom(&self) -> Option<&Box<QuadNode<T>>> {
        if let Some(children) = &self.children {
            return Some(&children[3]);
        }
        None
    }

    pub fn right_bottom(&self) -> Option<&Box<QuadNode<T>>> {
        if let Some(children) = &self.children {
            return Some(&children[2]);
        }
        None
    }

    pub fn get_bouding_box(&self) -> &Aabb2d {
        &self.boundary
    }

    fn subdivide(&mut self) {
        if self.children.is_some() {
            return;
        }

        let min = self.boundary.min;
        let max = self.boundary.max;
        let center = self.boundary.center();
        // 0 1
        // 3 2
        self.children = Some([
            Box::new(QuadNode::new(Aabb2d{
                min: Vec2::new(self.boundary.min.x, center.y),
                max: Vec2::new(center.x, max.y),
            })),
            Box::new(QuadNode::new(Aabb2d{
                min: center,
                max,
            })),
            Box::new(QuadNode::new(Aabb2d{
                min: Vec2::new(center.x, self.boundary.min.y),
                max: Vec2::new(max.x, center.y),
            })),
            Box::new(QuadNode::new(Aabb2d{
                min,
                max: center,
            })),
        ]);

        self.data = self.data.drain(..).filter_map(|t| {
            for child in self.children.as_mut().unwrap().iter_mut() {
                if child.boundary.contains(&t.aabb_2d()) {
                    child.insert(t);
                    return None;
                }
            }
            return Some(t);
        }).collect();
        
    }

    pub fn draw(&self, gizmos: &mut Gizmos, hue: Option<f32>) {
        let hue = hue.unwrap_or_else(|| 0.0);
        let color = Color::hsv(hue, 0.6, 0.6);

        gizmos.rect_2d(Isometry2d::from_translation(self.boundary.center()), self.boundary.half_size().xy() * 2.0, color);

        self.iter_data().for_each(|data| {
            gizmos.rect_2d(Isometry2d::from_translation(data.aabb_2d().center()), data.aabb_2d().half_size().xy() * 2.0, color);
        });

        self.iter_children().map(|children| children.for_each(|child| {
            child.draw(gizmos, Some((hue + 30.0) % 360.0));
        }));
    }

}



#[cfg(test)]
mod quadtree_tests {
    use bevy::math::bounding::Bounded2d;
    use std::ptr;

    use super::*;

   struct TestData(Rectangle, Vec2);

    impl QuadTreeData for TestData {
        fn aabb_2d(&self) -> Aabb2d {
            self.0.aabb_2d(Isometry2d::from_translation(self.1))
        }
    }

    #[test]
    fn test_insert() {
        let mut tree = QuadNode::new(Aabb2d{
            min: Vec2::new(0.0, 0.0),
            max: Vec2::new(10.0, 10.0),
        });

        let data1 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(4.0, 2.0)));
        let data1_ref = Arc::clone(&data1);
        tree.insert(data1);

        assert!(tree.children.is_none());
        assert!(tree.data.len() == 1);
        assert!(ptr::eq(tree.data[0].as_ref(), data1_ref.as_ref()));

        let data2 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(7.0, 7.0)));
        let data2_ref = Arc::clone(&data2);
        tree.insert(data2);


        assert!(tree.children.is_none());
        assert!(tree.data.len() == 2);
        assert!(ptr::eq(tree.data[1].as_ref(), data2_ref.as_ref()));

        let data3 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(3.0, 3.0)));
        let data3_ref = Arc::clone(&data3);
        tree.insert(data3);

        assert!(tree.children.is_some());
        assert!(tree.data.len() == 0);
        assert!(ptr::eq(tree.left_bottom().unwrap().data[0].as_ref(), data1_ref.as_ref()));
        assert!(ptr::eq(tree.right_top().unwrap().data[0].as_ref(), data2_ref.as_ref()));
        assert!(ptr::eq(tree.left_bottom().unwrap().data[1].as_ref(), data3_ref.as_ref()));

    }

    #[test]
    fn test_query() {
        let mut tree = QuadNode::new(Aabb2d{
            min: Vec2::new(0.0, 0.0),
            max: Vec2::new(10.0, 10.0),
        });
        let data1 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(4.0, 2.0)));
        let data1_ref = Arc::clone(&data1);
        tree.insert(data1);

        let data2 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(7.0, 7.0)));
        tree.insert(data2);

        let data3 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(3.0, 3.0)));
        let data3_ref = Arc::clone(&data3);
        tree.insert(data3);


        let query_boundary = Aabb2d{
            min: Vec2::new(2.0, 2.0),
            max: Vec2::new(6.0, 6.0),
        };

        let results = tree.query(&query_boundary);
        assert!(results.len() == 2);
        assert!(ptr::eq(results[0].as_ref(), data1_ref.as_ref()));
        assert!(ptr::eq(results[1].as_ref(), data3_ref.as_ref()));
    }


    #[test]
    fn test_remove() {
        let mut tree = QuadNode::new(Aabb2d{
            min: Vec2::new(0.0, 0.0),
            max: Vec2::new(10.0, 10.0),
        });
        let data1 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(4.0, 2.0)));
        let data1_ref = Arc::clone(&data1);
        tree.insert(data1);

        let data2 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(7.0, 7.0)));
        tree.insert(data2);

        let data3 = Arc::new(TestData(Rectangle::from_size(Vec2::new(1.0, 1.0)), Vec2::new(3.0, 3.0)));
        let data3_ref = Arc::clone(&data3);
        tree.insert(data3);

        let query_boundary = Aabb2d{
            min: Vec2::new(2.0, 2.0),
            max: Vec2::new(6.0, 6.0),
        };

        tree.remove(data1_ref.as_ref());
        let results = tree.query(&query_boundary);
        assert!(results.len() == 1);
        assert!(ptr::eq(results[0].as_ref(), data3_ref.as_ref()));

    }

}
