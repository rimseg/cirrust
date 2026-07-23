//! CardDAV: address-book discovery, contact fetch, and two-way contact editing,
//! exposed as Tauri commands. Same caching/refresh model as [`super::caldav`].

use super::dav::{href_to_dav_path, parse_multistatus};
use super::vcard::{self, Contact, ContactInput};
use super::{account_id, rand_id, store};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::webdav::WebDavClient;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

const VCARD_CT: &str = "text/vcard; charset=utf-8";

/// An address-book collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddressBookInfo {
    pub id: String,
    pub href: String,
    pub display_name: String,
    pub ctag: String,
}

async fn discover(client: &WebDavClient) -> AppResult<Vec<AddressBookInfo>> {
    let home = format!("addressbooks/users/{}", client.user());
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/" xmlns:c="urn:ietf:params:xml:ns:carddav">
  <d:prop>
    <d:resourcetype/>
    <d:displayname/>
    <cs:getctag/>
  </d:prop>
</d:propfind>"#;
    let xml = client.dav_propfind_raw(&home, "1", body.to_string()).await?;
    let responses = parse_multistatus(&xml)?;

    let mut out = Vec::new();
    for r in responses {
        if !r.is("addressbook") {
            continue;
        }
        let Some(dav_path) = href_to_dav_path(&r.href) else { continue };
        let id = dav_path.rsplit('/').next().unwrap_or(&dav_path).to_string();
        if id.is_empty() {
            continue;
        }
        out.push(AddressBookInfo {
            id: id.clone(),
            href: dav_path,
            display_name: r.prop("displayname").filter(|s| !s.is_empty()).unwrap_or(&id).to_string(),
            ctag: r.prop("getctag").unwrap_or_default().to_string(),
        });
    }
    Ok(out)
}

async fn fetch_contacts(
    client: &WebDavClient,
    ab: &AddressBookInfo,
) -> AppResult<Vec<Contact>> {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<c:addressbook-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:carddav">
  <d:prop><d:getetag/><c:address-data/></d:prop>
</c:addressbook-query>"#;
    let xml = client.dav_report_raw(&ab.href, "1", body.to_string()).await?;
    let responses = parse_multistatus(&xml)?;

    let mut contacts = Vec::new();
    for r in responses {
        let Some(data) = r.prop("address-data") else { continue };
        let Some(dav_path) = href_to_dav_path(&r.href) else { continue };
        let etag = r.prop("getetag").unwrap_or_default().to_string();
        if let Some(c) = vcard::parse_contact(data, &ab.id, &dav_path, &etag) {
            contacts.push(c);
        }
    }
    Ok(contacts)
}

#[tauri::command]
pub async fn carddav_addressbooks(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<Vec<AddressBookInfo>> {
    let id = account_id(&state).await?;
    match store::load::<Vec<AddressBookInfo>>(&app, &id, "addressbooks")? {
        Some(abs) if !abs.is_empty() => Ok(abs),
        _ => carddav_refresh(app.clone(), state).await,
    }
}

#[tauri::command]
pub async fn carddav_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<Vec<AddressBookInfo>> {
    let id = account_id(&state).await?;
    let client = state.client().await?;
    let books = discover(&client).await?;
    let previous =
        store::load::<Vec<AddressBookInfo>>(&app, &id, "addressbooks")?.unwrap_or_default();

    for ab in &books {
        let unchanged = previous
            .iter()
            .find(|p| p.id == ab.id)
            .map(|p| !p.ctag.is_empty() && p.ctag == ab.ctag)
            .unwrap_or(false);
        let cache_name = store::safe_name("contacts", &ab.id);
        let has_cache = store::load::<Vec<Contact>>(&app, &id, &cache_name)?.is_some();
        if unchanged && has_cache {
            continue;
        }
        let contacts = fetch_contacts(&client, ab).await?;
        store::save(&app, &id, &cache_name, &contacts)?;
    }

    store::save(&app, &id, "addressbooks", &books)?;
    Ok(books)
}

#[tauri::command]
pub async fn carddav_contacts(
    app: AppHandle,
    state: State<'_, AppState>,
    addressbook_ids: Option<Vec<String>>,
) -> AppResult<Vec<Contact>> {
    let id = account_id(&state).await?;
    let books = store::load::<Vec<AddressBookInfo>>(&app, &id, "addressbooks")?.unwrap_or_default();
    let wanted: Vec<&AddressBookInfo> = match &addressbook_ids {
        Some(ids) => books.iter().filter(|b| ids.contains(&b.id)).collect(),
        None => books.iter().collect(),
    };
    let mut all = Vec::new();
    for ab in wanted {
        let name = store::safe_name("contacts", &ab.id);
        if let Some(cs) = store::load::<Vec<Contact>>(&app, &id, &name)? {
            all.extend(cs);
        }
    }
    Ok(all)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveContactArgs {
    pub addressbook_id: String,
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    pub contact: ContactInput,
}

#[tauri::command]
pub async fn carddav_save_contact(
    app: AppHandle,
    state: State<'_, AppState>,
    args: SaveContactArgs,
) -> AppResult<Contact> {
    let id = account_id(&state).await?;
    let client = state.client().await?;
    let books = store::load::<Vec<AddressBookInfo>>(&app, &id, "addressbooks")?.unwrap_or_default();
    let ab = books
        .iter()
        .find(|b| b.id == args.addressbook_id)
        .ok_or_else(|| AppError::msg("unknown address book"))?;

    let (dav_path, vcf) = match (&args.href, &args.etag) {
        (Some(href), Some(etag)) => {
            let (_, existing) = client.dav_get_item(href).await?;
            let vcf = vcard::apply_edit(&existing, &args.contact)
                .ok_or_else(|| AppError::msg("contact body has no VCARD"))?;
            client.dav_put_update(href, VCARD_CT, vcf.clone(), etag).await?;
            (href.clone(), vcf)
        }
        _ => {
            let uid = format!("{}@cirrust", rand_id());
            let dav_path = format!("{}/{}.vcf", ab.href, uid);
            let vcf = vcard::build_new(&args.contact, &uid);
            client.dav_put_new(&dav_path, VCARD_CT, vcf.clone()).await?;
            (dav_path, vcf)
        }
    };

    let etag = client.dav_fetch_etag(&dav_path).await.unwrap_or_default();
    let contact = vcard::parse_contact(&vcf, &ab.id, &dav_path, &etag)
        .ok_or_else(|| AppError::msg("failed to parse saved contact"))?;
    upsert_cache(&app, &id, &ab.id, &contact.href, Some(contact.clone()))?;
    Ok(contact)
}

#[tauri::command]
pub async fn carddav_delete_contact(
    app: AppHandle,
    state: State<'_, AppState>,
    addressbook_id: String,
    href: String,
    etag: Option<String>,
) -> AppResult<()> {
    let id = account_id(&state).await?;
    let client = state.client().await?;
    client.dav_delete_item(&href, etag.as_deref().unwrap_or("")).await?;
    upsert_cache(&app, &id, &addressbook_id, &href, None)?;
    Ok(())
}

fn upsert_cache(
    app: &AppHandle,
    account: &str,
    addressbook_id: &str,
    href: &str,
    value: Option<Contact>,
) -> AppResult<()> {
    let name = store::safe_name("contacts", addressbook_id);
    let mut contacts = store::load::<Vec<Contact>>(app, account, &name)?.unwrap_or_default();
    contacts.retain(|c| c.href != href);
    if let Some(v) = value {
        contacts.push(v);
    }
    store::save(app, account, &name, &contacts)
}
