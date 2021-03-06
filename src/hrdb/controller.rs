use std::collections::HashMap;
use sha2::Digest;
use crate::logger::log;
use crate::hrdb::{
    branch::Branch,
    page::Page,
    content::Content,
    location::Location,
    shorthand::Shorthand,
    utils,
};

// exploration functions
pub async fn branches() -> Result<Vec<Location>, String> {
    Ok(
        utils::list("hrdb").await?
            .into_iter()
            .map(|n| Location::from_branch(n))
            .collect::<Vec<Location>>()
    )
}

pub async fn versions(location: Location) -> Result<Vec<Location>, String> {
    Ok(
        utils::list(&location.branch()).await?
            .into_iter()
            .map(|v| Location::from_branch_and_version(location.branch(), v))
            .collect::<Vec<Location>>()
    )
}

pub async fn head(location: Location) -> Result<Location, String> {
    Ok(
        versions(location).await?.last()
            .ok_or("No versions exist on this branch")?
            .to_owned()
    )
}

pub fn root(location: Location) -> Result<Location, String> {
    Ok(
        Location::from_branch_version_and_path(
            location.branch(),
            location.version()?,
            vec![location.version()?],
        )
    )
}

pub async fn children(location: Location) -> Result<Vec<Location>, String> {
    let page = Page::from(&location.end()?).await?;
    let mut c = vec![];

    for (_id, address) in page.children.into_iter() {
        c.push(location.forward(address)?);
    }
    return Ok(c);
}

pub async fn locate_id(version: Location, id: String) -> Result<Location, String> {
    // breadth-first search
    // would depth-first be faster?
    // they're most likely to be reading a leaf
    let mut queue = vec![root(version)?];

    while !queue.is_empty() {
        let location = queue.pop().unwrap();
        let top = location.end()?;
        let page = Page::from(&top).await?;
        if page.id() == id {
            return Ok(location);
        }
        for (id, address) in page.children {
            queue.push(location.forward(address)?)
        }
    }

    return Err("Could not locate a Page with that id for this version".to_owned());
}

pub async fn locate(version: Location, ids: Vec<String>) -> Result<Location, String> {
    let mut location = root(version)?;
    // log(&format!("ids: {:?}", ids));

    for id in ids[1..].iter() {
        let top = location.end()?;
        let page = Page::from(&top).await?;
        location = location.forward(
            page.children.get(id)
                .ok_or("Page does not have a child with that id")?
                .to_owned()
        )?;
    }

    return Ok(location);
}

// modification functions

pub async fn init() -> Result<(), String> {
    if let Ok(_) = utils::read("hrdb").await {
        return Ok(());
    }

    let content = Content::new("My friend ( ͡° ͜ʖ ͡°) would like you to check back soon.".to_owned());
    let address = utils::write(&content.to_string()).await?;

    let root = Page::new(
        "Home".to_owned(),
        address,
        HashMap::new(),
    );
    let version = utils::write(&root.to_string()?).await?;

    let mut table = HashMap::new();
    table.insert(root.short(), (0, root.id(), "".to_owned()));
    Shorthand::wrap(table).write().await?;

    utils::ensure("master").await?;
    utils::push("master", version.clone()).await?;
    utils::ensure("hrdb").await?;
    utils::push("hrdb", "master".to_owned()).await?;

    return Ok(());
}

pub async fn fork(from: Location, into: Location) -> Result<(), String> {
    // check that new branch is unique
    // to 'copy' into an existing branch, use merge
    if let Ok(_) = utils::read(&into.branch()).await {
        return Err("Can only fork into new branches".to_owned())
    }

    let from_branch  = Branch::from(&from.branch()).await?;
    let from_version = &from.version()?;
    let into_branch  = from_branch.fork(from_version, into.branch()).await?;

    utils::ensure(&into.branch()).await?;
    utils::append(&into.branch(), into_branch.versions).await?;

    return Ok(());
}

// pub async fn merge(location, location)

/// Updates page at location, iterating backwards through path chain.
async fn commit(location: Location, updated: Page) -> Result<(), String> {
    // commits can only be applied to the head version of the branch
    // check that this commit is being applied to the head
    let head    = head(location.clone()).await?;
    let version = location.version()?;
    let ver_no  = versions(location.clone()).await?.len(); // ver_no indexes versions.
    if version != head.version()? {
        // return Err(format!("head: {}, version: {}", head, version));
        return Err("Can only create a Page on latest version of a Branch".to_owned());
    }

    // load all the pages and update shorthand
    let mut pages = vec![];
    for address in location.path()?.iter() {
        let page = Page::from(address).await?;
        if location.branch() == "master" {
            Shorthand::update(page.short(), ver_no, page.id()).await?;
        }
        pages.push(page);
    }

    // Update the potentially changed title of the new page
    Shorthand::update(updated.short(), ver_no, updated.id()).await?;

    // get the new address of the page that has been updated
    let mut address   = utils::write(&updated.to_string()?).await?;
    let mut addresses = pages.drain(..).rev();
    let mut child     = addresses.next()
        .ok_or("Can not commit to a path without a Page")?;

    // iterate through parents backwards, updating new versions
    for mut parent in addresses {
        parent.children.insert(child.id(), address);
        address = utils::write(&parent.to_string()?).await?;
        child   = parent;
    }

    utils::push(&location.branch(), address).await?;
    return Ok(());
}

/// Creates a new page on the head of a branch,
/// returning the new page if successful
pub async fn create(
    location: Location,
    title: String,
    content: String,
    fields: HashMap<String, String>,
) -> Result<Location, String> {
    let c       = utils::write(&content.to_string()).await?;
    let new     = Page::new(title, c, fields);
    let address = utils::write(&new.to_string()?).await?;
    let child = location.forward(address)?;
    commit(child.clone(), new).await?;
    return Ok(child);
}

pub async fn edit(
    location: Location,
    title: Option<String>,
    content: Option<String>,
    fields: Option<HashMap<String, String>>,
) -> Result<(), String> {
    let mut page = Page::from(&location.end()?).await?;

    if let Some(c) = content { page.content = utils::write(&c.to_string()).await? }
    if let Some(t) = title   { page.title   = t }
    if let Some(f) = fields  { page.fields  = f }

    commit(location, page).await?;
    return Ok(());
}

pub async fn read(location: &Location) -> Result<(String, String, HashMap<String, String>), String> {
    let page = Page::from(&location.end()?).await?;

    log(&format!("title, {:?}; short: {:?}", page.title, page.short()));

    let title   = page.title;
    let content = utils::read(&page.content).await?;
    let fields  = page.fields;

    return Ok((title, content, fields));
}

pub async fn title(location: &Location) -> Result<String, String> {
    let page = Page::from(&location.end()?).await?;
    return Ok(page.title);
}

// more than just a create and delete.
// preserves id, commits to HRDB in safe order.
pub async fn relocate(from: Location, to: Location) -> Result<(), String> {
    let mut parent  = Page::from(&from.back()?.end()?).await?;
    let from_page   = Page::from(&from.end()?).await?;
    let mut to_page = Page::from(&to.end()?).await?;

    to_page.children.insert(from_page.id().clone(), from.end()?);
    parent.children.remove(&from_page.id());
    commit(to, to_page).await?;
    commit(from.back()?, parent).await?;

    return Ok(());
}

// looks like a simple read + write. too trivial to include.
// pub async fn duplicate(location: Location) -> Result<(), String> {
//     let (title, content, fields) = read(&location).await?;
//     create(location.back()?, title, content, fields).await?;
//     return Ok(());
// }

pub async fn delete(location: Location) -> Result<(), String> {
    let mut parent = Page::from(&location.back()?.end()?).await?;
    let page       = Page::from(&location.end()?).await?;
    parent.children.remove(&page.id());
    commit(location.back()?, parent).await?;
    return Ok(());
}
