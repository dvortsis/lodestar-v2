-- Rows whose text isn't all UTF-8, as a real SQL_ASCII bitmagnet holds.
-- convert_from(…, 'SQL_ASCII') stores the bytes unchanged. No real torrent
-- names or hashes. The expected digests are in tests/bitmagnet_check.rs.
--   01  ASCII                 Plain.ASCII.Name
--   02  Latin-1               Caf\xe9 Ol\xe9
--   03  GBK                   \xd6\xd0\xce\xc4  ("Chinese" in GBK)
--   04  UTF-8                 Caf\xc3\xa9
--   05  invalid sequences     \xc3\x28\xa0\xa1\xff\xfeA
INSERT INTO public.torrents (info_hash, name, size, private, created_at, updated_at, files_status, files_count) VALUES
  (decode(repeat('01', 20), 'hex'), convert_from('\x506c61696e2e41534349492e4e616d65'::bytea, 'SQL_ASCII'), 100, false, '2024-01-01', '2024-01-01', 'single', 1),
  (decode(repeat('02', 20), 'hex'), convert_from('\x436166e9204f6ce9'::bytea, 'SQL_ASCII'), 300, false, '2024-01-02', '2024-01-02', 'multi', 2),
  (decode(repeat('03', 20), 'hex'), convert_from('\xd6d0cec4'::bytea, 'SQL_ASCII'), 50, false, '2024-01-03', '2024-01-03', 'multi', 1),
  (decode(repeat('04', 20), 'hex'), convert_from('\x436166c3a9'::bytea, 'SQL_ASCII'), 70, true, '2024-01-04', '2024-01-04', 'multi', 1),
  (decode(repeat('05', 20), 'hex'), convert_from('\xc328a0a1fffe41'::bytea, 'SQL_ASCII'), 9, false, '2024-01-05', '2024-01-05', 'over_threshold', NULL);

-- Paths: Latin-1, Latin-1, GBK, UTF-8.
INSERT INTO public.torrent_files (info_hash, index, path, size, created_at, updated_at) VALUES
  (decode(repeat('02', 20), 'hex'), 0, convert_from('\x436166e92f30312e666c6163'::bytea, 'SQL_ASCII'), 200, '2024-01-02', '2024-01-02'),
  (decode(repeat('02', 20), 'hex'), 1, convert_from('\x436166e92f636f7665722e6a7067'::bytea, 'SQL_ASCII'), 100, '2024-01-02', '2024-01-02'),
  (decode(repeat('03', 20), 'hex'), 0, convert_from('\xd6d0cec42fcec4bcfe2e747874'::bytea, 'SQL_ASCII'), 50, '2024-01-03', '2024-01-03'),
  (decode(repeat('04', 20), 'hex'), 0, convert_from('\x436166c3a92f612e6d6b76'::bytea, 'SQL_ASCII'), 70, '2024-01-04', '2024-01-04');

INSERT INTO public.torrent_pieces (info_hash, piece_length, pieces, created_at) VALUES
  (decode(repeat('02', 20), 'hex'), 262144, decode(repeat('00', 40), 'hex'), '2024-01-02');
