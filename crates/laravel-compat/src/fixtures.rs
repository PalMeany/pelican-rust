//! Values produced by the original panel (PHP 8.5 / Laravel 13) inside the
//! difftest container with APP_KEY=base64:Rm9yRGlmZmVyZW50aWFsVGVzdGluZ09ubHkhIDMyQiE=

pub const ENCRYPT_STRING: &str = "eyJpdiI6IlpWUGRDcWVobU1KUXBMSWEzUzZoY1E9PSIsInZhbHVlIjoiaWt4Q3RmNUFTV1RWSW9KUzdiZTdJZz09IiwibWFjIjoiODQyMzM4NDYyNzU2OGFkNjQ5N2VjYmEyNzVkODZkZDdhYzBmZmI3ZjIyZTRkZTdlMzBmY2JlMjM5NjY2ZjllNCIsInRhZyI6IiJ9";
pub const ENCRYPT: &str = "eyJpdiI6IjBqSnlBZmN4bFEwNCt1bG9jV0haMnc9PSIsInZhbHVlIjoiVU13ZkR0UEQxRWNoK1M5dUg1bWR6M0ZtamJIZjFtZUF6TEZEYWdjNi9Oaz0iLCJtYWMiOiJjNjRlOWYwNTE1MTZiOWM5MDllNGQxNzMwNzVjNGI2ZWUxYWVmOWFkMmM1ZGIzNGMyMmU3OTAwNGQxYWVmNDI5IiwidGFnIjoiIn0=";
pub const ENCRYPT_INT_42: &str = "eyJpdiI6Ijc2bS9MbXdHbTZlK00vUkJrNkN6TXc9PSIsInZhbHVlIjoiamRlaEIrUHQrRFhDbTd6d0lDaHoyQT09IiwibWFjIjoiYjE3MTRjZDQ3NTM4NGUwNjkzM2FjNWI0Y2I4ZTc2ZGIwZTAzNWE2NjkzZDk1NGE4MThlYjhhOGQzZTcwMTA2MCIsInRhZyI6IiJ9";
/// encrypt(["a"=>1,"b"=>[true,null,1.5]])
pub const ENCRYPT_ARRAY: &str = "eyJpdiI6Ii8yOVlMclhqVUVpRFRaSkZVMlZFaHc9PSIsInZhbHVlIjoidXJhUTVNTllnd1U0Y3BjeGNVTTdSVGxhbmlkYng5ampHUllya3VnK1paUjN0Ny9ZZmpkUlM4U1NkSDdzbjZaNkxSRFhjbHNULzdydVBRa1pTalNRWmc9PSIsIm1hYyI6Ijk0MDUzNGQ2MGM2MGZmZTkyMTg3NGEwMzQzNjBjODdiMDY3NGI0NGEwM2Q0NTUyYTg4ZDQwYmQ4YmMxZGJkZWYiLCJ0YWciOiIifQ==";
/// Hash::make("password") with the default 12 rounds.
pub const BCRYPT_PASSWORD: &str = "$2y$12$bveDJMs40uE0PK2eM/j2.OdGEdD/2lPbSKfCcs.RIh4LfvA7zOARy";
/// serialize(["a"=>1,"b"=>[true,null,1.5],"u"=>"ü"])
pub const SERIALIZED: &[u8] =
    "a:3:{s:1:\"a\";i:1;s:1:\"b\";a:3:{i:0;b:1;i:1;N;i:2;d:1.5;}s:1:\"u\";s:2:\"\u{fc}\";}".as_bytes();
