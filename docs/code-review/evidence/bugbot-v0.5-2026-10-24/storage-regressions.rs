use lodestar_metainfo::{Info, Text, TextEncoding};
use lodestar_store::{Store, StoreConfig, NewTorrent, SourceId, flags, Refiled, Filtered, Held};
use std::time::{SystemTime, UNIX_EPOCH, Duration};
fn raw() -> Vec<u8> { b"d6:lengthi1e4:name13:Real Name.mkv12:piece lengthi16384e6:pieces20:00000000000000000000e".to_vec() }
fn add(store: &mut Store, info: &Info, raw: Option<&[u8]>, extra_flags:i64) -> i64 {
 store.insert(&[NewTorrent {info, raw_info:raw, discovered_at:1, metadata_at:2, extra_flags, source:SourceId::new(0).unwrap()}]).unwrap()[0].id()
}
#[test]
fn a_verified_partial_upgrade_archives_raw() {
 let mut s=Store::open_in_memory(StoreConfig{archive_raw:true,..StoreConfig::default()}).unwrap();
 let r=raw(); let real=Info::parse(&r).unwrap(); let mut partial=real.clone(); partial.files.clear();
 let id=add(&mut s,&partial,None,flags::IMPORTED|flags::FILES_PARTIAL);
 add(&mut s,&real,Some(&r),0);
 eprintln!("file_count={}, raw_present={}",s.torrent(id).unwrap().unwrap().file_count,s.raw_info(id).unwrap().is_some());
 assert!(s.raw_info(id).unwrap().is_some(),"verified fetch must populate enabled raw archive");
}
#[test]
fn a_verified_partial_upgrade_replaces_unverified_name() {
 let mut s=Store::open_in_memory(StoreConfig::default()).unwrap();
 let r=raw(); let real=Info::parse(&r).unwrap(); let mut partial=real.clone(); partial.files.clear(); partial.name=Text{raw:b"Old Imported Name".to_vec(),encoding:TextEncoding::UTF8};
 let id=add(&mut s,&partial,None,flags::IMPORTED|flags::FILES_PARTIAL);
 add(&mut s,&real,Some(&r),0);
 eprintln!("stored_name={}",s.torrent(id).unwrap().unwrap().name);
 assert_eq!(s.torrent(id).unwrap().unwrap().name,"Real Name.mkv");
}
#[test]
fn expired_refiling_still_purges_snapshot() {
 let dir=tempfile::tempdir().unwrap(); let db=dir.path().join("lodestar.db"); let snap=dir.path().join("lodestar.pre-v11.db");
 let mut s=Store::open(&db,StoreConfig::default()).unwrap(); let r=raw(); let real=Info::parse(&r).unwrap(); let id=add(&mut s,&real,Some(&r),0);
 s.connection().execute("VACUUM INTO ?1",[snap.to_str().unwrap()]).unwrap();
 let now=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
 std::fs::OpenOptions::new().write(true).open(&snap).unwrap().set_modified(UNIX_EPOCH+Duration::from_secs((now-60) as u64)).unwrap();
 let hashes=[real.primary_hash().0];
 let f=Filtered{hashes:&hashes,rule:"user-1",action:3,held:Some(Held::Quarantine),sealed:Some(b"sealed"),evidence:&[],at:now,expires_at:now+30*86400,adult_hold:false};
 let generation=s.class_gen().unwrap(); s.set_enrichments_at(generation,&[],&[Refiled{id,filtered:f,flags:0}]).unwrap();
 assert_eq!(s.count().unwrap(),0);
 let later=now+31*86400; s.purge_filter(later,0).unwrap();
 let report=s.purge_snapshots(later,0); eprintln!("purged_torrents={}, warnings={:?}",report.torrents,report.warnings);
 let old=Store::open_reader(&snap,StoreConfig::default()).unwrap();
 eprintln!("live_count={}, snapshot_count={}",s.count().unwrap(),old.count().unwrap());
 assert_eq!(old.count().unwrap(),0,"the live-deleted torrent must not survive snapshot retention purge");
}
