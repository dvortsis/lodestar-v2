use std::alloc::{GlobalAlloc,Layout,System};
use std::sync::{Arc,atomic::{AtomicIsize,Ordering}};
use lodestar_search::{SearchIndex,Lease,Sort,parse,CancelToken};
use lodestar_store::{Store,StoreConfig,NewTorrent,SourceId};
use lodestar_metainfo::{Info,InfoHash};
struct Tracking;
static LIVE:AtomicIsize=AtomicIsize::new(0);
unsafe impl GlobalAlloc for Tracking {
 unsafe fn alloc(&self,l:Layout)->*mut u8 {let p=unsafe{System.alloc(l)}; if !p.is_null(){LIVE.fetch_add(l.size() as isize,Ordering::Relaxed);} p}
 unsafe fn dealloc(&self,p:*mut u8,l:Layout) {LIVE.fetch_sub(l.size() as isize,Ordering::Relaxed);unsafe{System.dealloc(p,l)}}
 unsafe fn realloc(&self,p:*mut u8,l:Layout,n:usize)->*mut u8 {let p=unsafe{System.realloc(p,l,n)};if !p.is_null(){LIVE.fetch_add(n as isize-l.size() as isize,Ordering::Relaxed);}p}
}
#[global_allocator] static A:Tracking=Tracking;
fn main(){
 let r=b"d6:lengthi10000000e4:name13:Real Name.mkv12:piece lengthi16384e6:pieces20:00000000000000000000e";
 let real=Info::parse(r).unwrap();
 let mut infos=Vec::new(); for id in 1..=10000u64 {let mut i=real.clone();let mut hash=[0u8;20];hash[..8].copy_from_slice(&id.to_le_bytes());i.v1=Some(InfoHash(hash));infos.push(i);}
 let mut store=Store::open_in_memory(StoreConfig{compression_level:1,..StoreConfig::default()}).unwrap();
 let batch:Vec<_>=infos.iter().map(|info|NewTorrent{info,raw_info:None,discovered_at:1,metadata_at:1,extra_flags:0,source:SourceId::new(0).unwrap()}).collect();
 store.insert(&batch).unwrap(); let index=Arc::new(SearchIndex::in_memory().unwrap());let mut writer=index.writer(15<<20).unwrap();writer.catch_up(&mut store,10001).unwrap();writer.commit().unwrap();writer.finish().unwrap();
 let token=CancelToken::new();token.cancel();
 index.clear_cache();let baseline=LIVE.load(Ordering::Relaxed);
 for n in 0..100 {let ast=parse(&format!("file:(size:>{n})")).unwrap();let result=Lease::new(index.clone(),&ast,1,Sort::Relevance,&token);assert!(result.is_err());}
 let retained=LIVE.load(Ordering::Relaxed)-baseline;let before_clear=LIVE.load(Ordering::Relaxed);index.clear_cache();let freed=before_clear-LIVE.load(Ordering::Relaxed);
 println!("100 cancelled cold file queries over10000 torrents: retained={retained} bytes; clear_cache released={freed} bytes");
 assert!(retained<8<<20,"cancelled searches retained over8MiB of hidden file-set caches");
}
