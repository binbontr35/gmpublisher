use std::{collections::{HashMap, HashSet}, fmt::{Display, Write}, fs::File, hash::Hash, io::{Read, BufReader, BufWriter}, path::PathBuf, sync::Arc};

lazy_static! {
	static ref RE_BUNDLE_DATA: Regex = regex::RegexBuilder::new(r#"^[ \t]*(?:(?:("|'|\[(=*)\[)(\d+)(?:\1|\]\2\]))|--#[ \t]*+(.+?)(?:[ \t]+(.+)|$))"#).multi_line(true).build().unwrap();
}

use chrono::Utc;
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use steamworks::{QueryResults, PublishedFileId};
use thiserror::Error;

use crate::Transaction;

#[derive(Debug, Clone, Serialize, Error)]
enum BundleError {
	#[error("ERR_IO_ERROR")]
	IoError,

	#[error("ERR_BUNDLE_PARSE_ERROR")]
	ParseError,

	#[error("ERR_BUNDLE_NO_ITEMS_FOUND")]
	NoItemsFound,

	#[error("ERR_SERIALIZATION")]
	Serialization,

	#[error("ERR_STEAM_ERROR:{0}")]
	SteamError(steamworks::SteamError),

	#[error("ERR_INVALID_COLLECTION")]
	InvalidCollection,
}
impl From<std::io::Error> for BundleError {
    fn from(_: std::io::Error) -> Self {
        BundleError::IoError
    }
}
impl From<Box<bincode::ErrorKind>> for BundleError {
    fn from(error: Box<bincode::ErrorKind>) -> Self {
        match *error {
            bincode::ErrorKind::Io(_) | bincode::ErrorKind::SizeLimit => BundleError::IoError,
			_ => BundleError::Serialization
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct BundleCollectionLink {
	id: PublishedFileId,
	include: Vec<PublishedFileId>,
	exclude: Vec<PublishedFileId>
}

#[derive(Serialize, Deserialize, Debug)]
struct Bundle {
	id: u32,
	name: String,
	updated: chrono::DateTime<Utc>,
	collection: Option<BundleCollectionLink>,
	items: Vec<PublishedFileId>,
}
impl PartialEq for Bundle {
	fn eq(&self, other: &Self) -> bool {
		self.id == other.id
	}
}
impl Eq for Bundle {}
impl PartialOrd for Bundle {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		self.updated.partial_cmp(&other.updated)
	}
}
impl Ord for Bundle {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.updated.cmp(&other.updated)
	}
}
impl Bundle {
	pub fn import(src: String) -> Result<Bundle, BundleError> {
		let mut bundle_start = false;

		let mut name = String::new();
		let mut collection: Option<BundleCollectionLink> = None;
		let mut updated = chrono::Utc::now();
		let mut items = Vec::with_capacity(4096);

		for data in RE_BUNDLE_DATA.captures_iter(&src) {
			if let Some(key) = data.get(4) {
				if key.as_str() == "bundle" {
					if !bundle_start {
						bundle_start = true;
					} else {
						break;
					}
				} else {
					let val = match data.get(5) {
						Some(val) => val,
						None => continue
					};
					match key.as_str() {
						"name" => name = val.as_str().to_string(),
						"collection" => if let Ok(id) = val.as_str().parse::<u64>() {
							collection = Some(BundleCollectionLink {
								id: PublishedFileId(id),
								include: Vec::with_capacity(4096),
								exclude: Vec::new(),
							});
						},
						"updated" => if let Ok(parsed) = chrono::DateTime::parse_from_rfc2822(val.as_str()) {
							updated = parsed.with_timezone(&Utc);
						},
						_ => {}
					}
				}
			} else if let Some(item) = data.get(3) {
				items.push(PublishedFileId(match item.as_str().parse::<u64>() {
					Ok(id) => id,
					Err(_) => continue
				}));
			} else {
				#[cfg(debug_assertions)]
				panic!("Unexpected match when parsing bundle data");
			}
		}

		if items.is_empty() {
			return Err(BundleError::NoItemsFound);
		}

		if let Some(ref mut collection) = collection {
			if let Some(collection_items) = steam!().fetch_collection_items(collection.id) {
				for item in collection_items {
					match items.binary_search(&item) {
						Ok(pos) => {
							items.remove(pos);
							collection.include.push(item);
						},
						Err(_) => {
							collection.exclude.push(item);
						}
					}
				}
				collection.include.shrink_to_fit();
				collection.exclude.shrink_to_fit();
			}
		}

		let id = BUNDLES.lock().id(); // TODO potential deadlock?
		Ok(Bundle {
			id,
			name,
			updated,
			collection,
			items,
		})
	}

	pub fn export(&self, item_names: HashMap<PublishedFileId, String>, collection_name: Option<&str>) -> String {
		let mut export = String::with_capacity(1000000);
		write!(&mut export, "-- generated by gmpublisher\n").unwrap();
		write!(&mut export, "-- https://gmpublisher.download\n").unwrap();
		write!(&mut export, "--# bundle\n").unwrap();

		write!(&mut export, "--# name {}\n", self.name).unwrap();

		if let Some(ref collection) = self.collection {
			write!(&mut export, "--# collection {}\n", collection.id.0.to_string()).unwrap();
		}

		write!(&mut export, "--# updated {}\n", self.updated.to_rfc2822()).unwrap();

		write!(&mut export, "for _,w in ipairs({{\n\n").unwrap();

		for item in self.items.iter() {
			write!(&mut export, "\"{}\"", item.0.to_string()).unwrap();
			if let Some(name) = item_names.get(item) {
				write!(&mut export, " -- {}\n", name).unwrap();
			}
		}

		if let Some(ref collection) = self.collection {
			write!(&mut export, "\n-- Collection\n").unwrap();
			if let Some(collection_name) = collection_name {
				write!(&mut export, "-- {}\n", collection_name).unwrap();
			}
			write!(&mut export, "-- https://steamcommunity.com/sharedfiles/filedetails/?id={}\n", collection.id.0.to_string()).unwrap();
			for item in collection.include.iter() {
				write!(&mut export, "\"{}\"", item.0.to_string()).unwrap();
				if let Some(name) = item_names.get(item) {
					write!(&mut export, " -- {}\n", name).unwrap();
				}
			}
		}

		write!(&mut export, "\n}}) do resource.AddWorkshop(w) end").unwrap();

		export.shrink_to_fit();
		export
	}
}

#[derive(Serialize, Deserialize)]
pub struct Bundles {
	saved: Vec<Arc<Bundle>>,
	id: u32,
}
impl Bundles {
	pub fn init() -> Bundles {
		let mut saved = Vec::new();
		let mut id = 0;

		std::fs::create_dir_all(&*bundles_path()).expect("Failed to create content generator bundles directory");

		if let Ok(dir) = bundles_path().read_dir() {
			for entry in dir {
				ignore! { try_block!({
					let entry = entry?;
					let contents: Arc<Bundle> = Arc::new(bincode::deserialize_from(BufReader::new(File::open(entry.path())?))?);
					id = id.max(contents.id);

					saved.insert(
						match saved.binary_search(&contents) {
							Ok(pos) => pos,
							Err(pos) => pos,
						},
						contents
					);
				}) };
			}
		}

		Bundles { saved, id }
	}

	pub fn id(&mut self) -> u32 {
		self.id += 1;
		self.id
	}
}

lazy_static! {
	pub static ref BUNDLES: Mutex<Bundles> = Mutex::new(Bundles::init());
}

fn bundles_path() -> PathBuf {
	app_data!().user_data_dir().join("bundles")
}

#[tauri::command]
fn get_bundles() -> &'static Vec<Arc<Bundle>> {
	unsafe { &*(&BUNDLES.lock().saved as *const _) }
}

#[tauri::command]
fn update_bundle(bundle: Bundle) -> bool {
	try_block!({
		let mut content_generator = BUNDLES.lock();

		let f = File::create(bundles_path().join(bundle.id.to_string()))?;
		bincode::serialize_into(BufWriter::new(f), &bundle)?;

		let bundle = Arc::new(bundle);

		match content_generator.saved.binary_search(&bundle) {
			Ok(pos) => content_generator.saved[pos] = bundle,
			Err(pos) => content_generator.saved.insert(pos, bundle),
		}
	})
	.is_ok()
}

#[tauri::command]
fn import_bundle(path: PathBuf) -> Transaction {
	let transaction = transaction!();
	if path.is_file() {
		if let Ok(src) = std::fs::read_to_string(path) {
			match Bundle::import(src) {
				Ok(bundle) => transaction.finished(bundle),
				Err(error) => transaction.error(error, turbonone!())
			}
			return transaction;
		}
	}

	transaction.error(crate::gma::GMAError::IOError, turbonone!());
	transaction
}

#[tauri::command]
fn paste_bundle(pasted: String) -> Transaction {
	let transaction = transaction!();
	match Bundle::import(pasted) {
		Ok(bundle) => transaction.finished(bundle),
		Err(error) => transaction.error(error, turbonone!())
	}
	transaction
}

#[tauri::command]
fn new_bundle(name: String, based_on_collection: Option<PublishedFileId>) -> Result<Arc<Bundle>, BundleError> {
	let mut bundles = BUNDLES.lock();
	bundles.id += 1;

	let id = bundles.id;

	println!("based_on_collection {:?}", based_on_collection);

	let bundle = Bundle {
		id,
		name,
		updated: chrono::Utc::now(),
		collection: match based_on_collection {
			Some(id) => {
				let collection = check_bundle_collection(id, Some(true))?;
				Some(BundleCollectionLink {
					id,
					include: collection.items,
					exclude: Vec::with_capacity(4096),
				})
			},
			None => None
		},
		items: Vec::with_capacity(4096),
	};

	println!("{:#?}", bundle);

	let mut path = bundles_path();
	path.push(id.to_string());
	std::fs::write(path, bincode::serialize(&bundle)?)?;

	let bundle = Arc::new(bundle);
	bundles.saved.push(bundle.clone());
	Ok(bundle)
}

#[derive(Serialize, Debug)]
struct CollectionData {
	title: String,
	preview_url: Option<String>,
	items: Vec<PublishedFileId>
}
#[tauri::command]
fn check_bundle_collection(collection: PublishedFileId, with_children: Option<bool>) -> Result<CollectionData, BundleError> {
	let collection_result = Arc::new(Mutex::new(None));

	let collection_result_ref = collection_result.clone();
	steam!().client().ugc().query_item(collection).unwrap().include_children(true).allow_cached_response(600).fetch(move |result: Result<QueryResults<'_>, steamworks::SteamError>| {
		match result {
			Ok(result) => {
				if let Some(item) = result.get(0) {
					if item.file_type == steamworks::FileType::Collection {
						if let Some(children) = result.get_children(0) {
							*collection_result_ref.lock() = Some(Ok(CollectionData {
								title: item.title,
								preview_url: result.preview_url(0),
								items: if with_children.unwrap_or(false) { children } else { Vec::new() }
							}));
							return;
						}
					}
				}
				*collection_result_ref.lock() = Some(Err(BundleError::InvalidCollection));
			},

			Err(error) => *collection_result_ref.lock() = Some(Err(BundleError::SteamError(error)))
		}
	});

	loop {
		if let Some(lock) = collection_result.try_lock() {
			if lock.is_some() { break; }
		}
		sleep_ms!(25);
	}

	Arc::try_unwrap(collection_result).unwrap().into_inner().unwrap()
}
