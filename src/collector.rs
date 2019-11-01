use crate::frames::UnresolvedFrames;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::marker::PhantomData;

pub const BUCKETS: usize = (1 << 12) / std::mem::size_of::<Entry<UnresolvedFrames>>();
pub const BUCKETS_ASSOCIATIVITY: usize = 4;
pub const BUFFER_LENGTH: usize = (1 << 18) / std::mem::size_of::<Entry<UnresolvedFrames>>();

pub struct Entry<T> {
    pub item: T,
    pub count: usize,
}

pub struct Bucket<T> {
    pub length: usize,
    entries: [Entry<T>; BUCKETS_ASSOCIATIVITY],
}

impl<T: Eq> Default for Bucket<T> {
    fn default() -> Bucket<T> {
        Self {
            length: 0,
            entries: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
        }
    }
}

impl<T: Eq> Bucket<T> {
    pub fn add(&mut self, key: T) -> Option<Entry<T>> {
        let mut done = false;
        self.entries[0..self.length].iter_mut().for_each(|ele| {
            if ele.item == key {
                ele.count += 1;
                done = true;
            }
        });

        if done {
            None
        } else if self.length < BUCKETS_ASSOCIATIVITY {
            let ele = &mut self.entries[self.length];
            ele.item = key;
            ele.count = 1;

            self.length += 1;
            None
        } else {
            let mut min_index = 0;
            let mut min_count = self.entries[0].count;
            for index in 0..self.length {
                let count = self.entries[index].count;
                if count < min_count {
                    min_index = index;
                    min_count = count;
                }
            }

            let mut new_entry = Entry {
                item: key,
                count: 1,
            };
            std::mem::swap(&mut self.entries[min_index], &mut new_entry);
            Some(new_entry)
        }
    }

    pub fn iter(&self) -> BucketIterator<T> {
        BucketIterator::<T> {
            related_bucket: &self,
            index: 0,
        }
    }
}

pub struct BucketIterator<'a, T> {
    related_bucket: &'a Bucket<T>,
    index: usize,
}

impl<'a, T> Iterator for BucketIterator<'a, T> {
    type Item = &'a Entry<T>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.related_bucket.length {
            self.index += 1;
            Some(&self.related_bucket.entries[self.index - 1])
        } else {
            None
        }
    }
}

pub struct StackHashCounter<T: Hash + Eq> {
    buckets: [Bucket<T>; BUCKETS],
}

impl<T: Hash + Eq> Default for StackHashCounter<T> {
    fn default() -> Self {
        let mut counter = Self {
            buckets: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
        };
        counter.buckets.iter_mut().for_each(|item| {
            *item = Bucket::<T>::default();
        });

        counter
    }
}

impl<T: Hash + Eq> StackHashCounter<T> {
    fn hash(key: &T) -> u64 {
        let mut s = DefaultHasher::new();
        key.hash(&mut s);
        s.finish()
    }

    pub fn add(&mut self, key: T) -> Option<Entry<T>> {
        let hash_value = Self::hash(&key);
        let bucket = &mut self.buckets[(hash_value % BUCKETS as u64) as usize];

        bucket.add(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Entry<T>> {
        let mut iter: Box<dyn Iterator<Item = &Entry<T>>> =
            Box::new(self.buckets[0].iter().chain(std::iter::empty()));
        for bucket in self.buckets[1..].iter() {
            iter = Box::new(iter.chain(bucket.iter()));
        }

        iter
    }
}

pub struct TempFdArray<T> {
    file: File,
    buffer: [T; BUFFER_LENGTH],
    buffer_index: usize,
    phantom: PhantomData<T>,
}

impl<T> TempFdArray<T> {
    fn new() -> std::io::Result<TempFdArray<T>> {
        let file = tempfile::tempfile()?;
        Ok(Self {
            file,
            buffer: unsafe { std::mem::MaybeUninit::uninit().assume_init() },
            buffer_index: 0,
            phantom: PhantomData,
        })
    }

    fn flush_buffer(&mut self) -> std::io::Result<()> {
        self.buffer_index = 0;
        let buf = unsafe {
            std::slice::from_raw_parts(
                self.buffer.as_ptr() as *const u8,
                BUFFER_LENGTH * std::mem::size_of::<T>(),
            )
        };
        self.file.write_all(buf)?;

        Ok(())
    }

    fn push(&mut self, entry: T) -> std::io::Result<()> {
        if self.buffer_index >= BUFFER_LENGTH {
            self.flush_buffer()?;
        }

        self.buffer[self.buffer_index] = entry;
        self.buffer_index += 1;

        Ok(())
    }

    fn iter(&mut self) -> std::io::Result<impl Iterator<Item = &T>> {
        let mut file_vec = Vec::new();
        self.file.read_to_end(&mut file_vec)?;

        let length = file_vec.len() / std::mem::size_of::<T>();
        let ts = unsafe { std::slice::from_raw_parts(file_vec.as_ptr() as *const T, length) };

        let buf_len = self.buffer_index;
        Ok(self.buffer[0..buf_len].iter().chain(ts.iter()))
    }
}

pub struct Collector<T: Hash + Eq> {
    map: StackHashCounter<T>,
    temp_array: TempFdArray<Entry<T>>,
}

impl<T: Hash + Eq> Collector<T> {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            map: StackHashCounter::<T>::default(),
            temp_array: TempFdArray::<Entry<T>>::new()?,
        })
    }

    pub fn add(&mut self, key: T) -> std::io::Result<()> {
        if let Some(evict) = self.map.add(key) {
            self.temp_array.push(evict)?;
        }

        Ok(())
    }

    pub fn iter(&mut self) -> std::io::Result<impl Iterator<Item = &Entry<T>>> {
        Ok(self.map.iter().chain(self.temp_array.iter()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn stack_hash_counter() {
        let mut stack_hash_counter = StackHashCounter::<usize>::default();
        stack_hash_counter.add(0);
        stack_hash_counter.add(1);
        stack_hash_counter.add(1);

        stack_hash_counter.iter().for_each(|item| {
            if item.item == 0 {
                assert_eq!(item.count, 1);
            } else if item.item == 1 {
                assert_eq!(item.count, 2);
            } else {
                unreachable!();
            }
        });
    }

    fn add_map(hashmap: &mut BTreeMap<usize, usize>, entry: &Entry<usize>) {
        match hashmap.get_mut(&entry.item) {
            None => {
                hashmap.insert(entry.item, entry.count);
            }
            Some(count) => *count += entry.count,
        }
    }

    #[test]
    fn evict_test() {
        let mut stack_hash_counter = StackHashCounter::<usize>::default();
        let mut real_map = BTreeMap::new();

        for item in 0..(1 << 10) * 4 {
            for _ in 0..(item % 4) {
                match stack_hash_counter.add(item) {
                    None => {}
                    Some(evict) => {
                        add_map(&mut real_map, &evict);
                    }
                }
            }
        }

        stack_hash_counter.iter().for_each(|entry| {
            add_map(&mut real_map, &entry);
        });

        for item in 0..(1 << 10) * 4 {
            let count = item % 4;
            match real_map.get(&item) {
                Some(item) => {
                    assert_eq!(*item, count);
                }
                None => {
                    assert_eq!(count, 0);
                }
            }
        }
    }

    #[test]
    fn collector_test() {
        let mut collector = Collector::new().unwrap();
        let mut real_map = BTreeMap::new();

        for item in 0..(1 << 10) * 4 {
            for _ in 0..(item % 4) {
                collector.add(item);
            }
        }

        collector.iter().unwrap().for_each(|entry| {
            add_map(&mut real_map, &entry);
        });

        for item in 0..(1 << 10) * 4 {
            let count = item % 4;
            match real_map.get(&item) {
                Some(item) => {
                    assert_eq!(*item, count);
                }
                None => {
                    assert_eq!(count, 0);
                }
            }
        }
        assert!(false);
    }
}