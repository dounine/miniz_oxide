use miniz_oxide::deflate::core::compress_to_output;
use miniz_oxide::deflate::{compress_to_vec, compress_to_vec_callback};
use miniz_oxide::inflate::{
    decompress_to_vec, decompress_to_vec_callback, decompress_to_vec_with_limit,
};
use std::fs;
use std::ops::{Deref, DerefMut, Index, IndexMut, Range, RangeFrom, RangeFull};

pub struct OffsetVec<T> {
    inner: Vec<T>,
    virtual_start: usize, // 虚拟起始索引
}

impl<T> OffsetVec<T> {
    pub fn len(&self) -> usize {
        self.virtual_start + self.inner.len()
    }
}

impl<T> OffsetVec<T> {
    pub fn new(virtual_start: usize) -> Self {
        Self {
            inner: Vec::new(),
            virtual_start, // 固定虚拟偏移量
        }
    }

    // 逻辑索引转物理索引
    #[inline]
    fn adjust_index(&self, index: usize) -> usize {
        index - self.virtual_start
    }

    // 物理索引有效性检查
    #[inline]
    fn check_bounds(&self, index: usize) {
        assert!(
            index >= self.virtual_start,
            "Index {} < virtual start {}",
            index,
            self.virtual_start
        );
    }
}
impl<T> Index<Range<usize>> for OffsetVec<T> {
    type Output = [T];

    fn index(&self, range: Range<usize>) -> &Self::Output {
        self.check_bounds(range.start);
        let adj_start = self.adjust_index(range.start);
        let adj_end = self.adjust_index(range.end);
        &self.inner[adj_start..adj_end]
    }
}
impl<T> Index<RangeFull> for OffsetVec<T> {
    type Output = [T];

    fn index(&self, _range: RangeFull) -> &Self::Output {
        &self.inner[..]
    }
}
impl<T> Index<RangeFrom<usize>> for OffsetVec<T> {
    type Output = [T];

    fn index(&self, range: RangeFrom<usize>) -> &Self::Output {
        self.check_bounds(range.start);
        let adj_start = self.adjust_index(range.start);
        &self.inner[adj_start..]
    }
}
impl<T> IndexMut<Range<usize>> for OffsetVec<T> {
    fn index_mut(&mut self, range: Range<usize>) -> &mut Self::Output {
        self.check_bounds(range.start);
        let adj_start = self.adjust_index(range.start);
        let adj_end = self.adjust_index(range.end);
        &mut self.inner[adj_start..adj_end]
    }
}
impl<T> IndexMut<RangeFrom<usize>> for OffsetVec<T> {
    fn index_mut(&mut self, range: RangeFrom<usize>) -> &mut Self::Output {
        self.check_bounds(range.start);
        let adj_start = self.adjust_index(range.start);
        &mut self.inner[adj_start..]
    }
}
impl<T> Deref for OffsetVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.inner[..]
    }
}
impl<T> DerefMut for OffsetVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner[..]
    }
}

fn main() {
    // let mut list: OffsetVec<u8> = OffsetVec::new(10);
    // list.inner.extend_from_slice(&[1, 2, 3]);
    //
    //
    // // let data = &list[10..];
    // // let size = list.len();
    // println!("data {:?} {}", data, size);
    // if true {
    //     return;
    // }
    let origin = fs::read("./data/wt_bg.mp3").unwrap();
    println!("origin size {}", origin.len());
    let data1 = compress_to_vec(&origin, 6);
    let data2 = compress_to_vec_callback(&origin, 6, 1024 * 1024, |compress_size| {
        // println!("compress size {}", compress_size)
    });
    assert_eq!(data1, data2);
    // println!("compress size {}",data.len());
    // fs::write("./data/hi2.zip", data).unwrap();
    // println!("compress size {}",data.len());
    // let data = decompress_to_vec(&data2).unwrap();
    println!("compress size {}", data1.len());
    let mut total = 0;
    let mut index = 0;
    let data = decompress_to_vec_callback(&data2, |decompress_size| {
        total += decompress_size;
        println!(
            "decompres size {} {} total:{}",
            index, decompress_size, total
        );
        index += 1;
    })
    .unwrap();
    assert_eq!(origin, data);
    println!("size {}", total)
    // println!("Hello, world!");
}
