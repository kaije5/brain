//! In-memory [`KnowledgeProvider`] fake shared by SCRUM-87 integration tests.
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use cortex_application::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
    KnowledgeUpdate, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
    ProviderRead,
};
use cortex_domain::{
    ContentHash, ObservedRevision, ProviderProvenance, ProviderResourceKind, ProviderResourceRef,
    WorkspaceId,
};

#[derive(Clone, Default)]
pub struct FakeKnowledgeProvider {
    documents: Arc<Mutex<BTreeMap<String, KnowledgeDocument>>>,
    fail_creates: Arc<Mutex<bool>>,
    next_ordinal: Arc<Mutex<u64>>,
}

fn knowledge_resource(workspace_id: WorkspaceId, resource_id: &str) -> ProviderResourceRef {
    ProviderResourceRef::new(
        workspace_id,
        cortex_domain::ProviderId::new("markdown-vault").expect("provider id"),
        cortex_domain::ProviderResourceId::new(resource_id).expect("resource id"),
        ProviderResourceKind::Knowledge,
    )
}

impl FakeKnowledgeProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_fail_creates(&self, fail: bool) {
        *self.fail_creates.lock().expect("fail mutex") = fail;
    }

    pub fn document_count(&self) -> usize {
        self.documents.lock().expect("documents mutex").len()
    }

    pub fn contains_title(&self, title: &str) -> bool {
        self.documents
            .lock()
            .expect("documents mutex")
            .values()
            .any(|document| document.title() == title)
    }
}

impl KnowledgeProvider for FakeKnowledgeProvider {
    async fn get(
        &self,
        _resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
        unimplemented!("not exercised by review flows under test")
    }

    async fn search(
        &self,
        _query: &KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
        unimplemented!("not exercised by review flows under test")
    }

    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        if *self.fail_creates.lock().expect("fail mutex") {
            return Err(ProviderError::Unavailable);
        }
        let mut next = self.next_ordinal.lock().expect("ordinal mutex");
        *next += 1;
        let revision = *next;
        drop(next);
        let resource = knowledge_resource(
            input.workspace_id(),
            &format!("reviews/review-{revision}.md"),
        );
        let document = KnowledgeDocument::new(
            ProviderProvenance::new(
                resource.clone(),
                ObservedRevision::new(format!("rev-{revision}")).expect("revision"),
                ContentHash::new([revision as u8; 32]),
            ),
            input.title(),
            input.body(),
        )
        .expect("document");
        let key = resource.resource_id().as_str().to_owned();
        self.documents
            .lock()
            .expect("documents mutex")
            .insert(key, document);
        let current = self
            .documents
            .lock()
            .expect("documents mutex")
            .values()
            .last()
            .expect("inserted")
            .provenance()
            .clone();
        Ok(ProviderMutation::created(current))
    }

    async fn update(&self, _input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        Err(ProviderError::Internal)
    }

    async fn delete(&self, _input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        Err(ProviderError::Internal)
    }
}
