use dandelion_core::wiki::{draft_requested_reply,list_drafts};
#[test]
fn explicit_save_without_project_is_durable_utf8_safe_and_idempotent(){
 let tmp=tempfile::tempdir().unwrap();
 let text="Go 核心知识：并发与类型系统。".repeat(10);
 let first=draft_requested_reply(tmp.path(),"session","answer","整理 Go 核心知识，然后在知识中进行学习",&text,120).unwrap();
 assert!(first.len()>1);
 assert_eq!(first.iter().map(|d|d.content.as_str()).collect::<String>(),text);
 assert!(first.iter().all(|d|d.content.len()<=120));
 let second=draft_requested_reply(tmp.path(),"session","answer","保存到 Wiki",&text,120).unwrap();
 assert_eq!(first.len(),second.len());
 assert_eq!(list_drafts(tmp.path(),"session").len(),first.len());
 assert!(draft_requested_reply(tmp.path(),"other","x","解释 Go 基础知识",&text,120).unwrap().is_empty());
 assert!(draft_requested_reply(tmp.path(),"other","negative","不要保存到知识库，只学习概念",&text,120).unwrap().is_empty());
 assert!(list_drafts(tmp.path(),"other").is_empty());
}

#[tokio::test]
async fn native_answer_without_a_write_tool_still_preserves_the_requested_draft(){
 use std::sync::Arc;
 use dandelion_core::{Runtime,Settings,PromptRequest};
 use dandelion_core::host::CliHost;
 use dandelion_core::provider::scripted::{ScriptedProvider,ScriptedTurn};
 let tmp=tempfile::tempdir().unwrap();let data=tmp.path().join("data");
 let mut settings=Settings::defaults_configured();settings.workspace=Some(tmp.path().into());settings.cron_tick_sec=3600;
 let rt=Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.clone())),settings,Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("Go 核心知识：goroutine 与 channel 用于并发协作。")]))).unwrap();
 let sid=rt.create_session(None).unwrap();
 rt.prompt(PromptRequest::new(sid.clone(),"整理 Go 核心知识，保存到知识库")).await.unwrap();
 let drafts=list_drafts(&data,sid.as_str());assert_eq!(drafts.len(),1);assert!(drafts[0].content.contains("goroutine"));
}

#[test]
fn model_project_names_are_one_portable_folder_component(){
 use dandelion_core::wiki::validate_project_folder_name;
 assert_eq!(validate_project_folder_name(" Go语言学习 ").unwrap(),"Go语言学习");
 assert_eq!(dandelion_core::wiki::project_name_from_model_reply("```json\n{\"name\":\"Go语言学习\"}\n```").unwrap(),"Go语言学习");
 assert!(dandelion_core::wiki::project_name_from_model_reply(r#"{"name":"../outside"}"#).is_err());
 for name in ["..",".","../other","a/b","a\\b","CON","NUL.txt","x\nname","project.",""] {assert!(validate_project_folder_name(name).is_err(),"{name}");}
}

#[tokio::test]
async fn model_name_then_folder_confirmation_publishes_readable_wiki(){
 use std::sync::Arc;
 use dandelion_core::{Runtime,Settings};
 use dandelion_core::host::CliHost;
 use dandelion_core::provider::scripted::{ScriptedProvider,ScriptedTurn};
 use dandelion_core::wiki::WikiDraftTarget;
 let tmp=tempfile::tempdir().unwrap();let data=tmp.path().join("data");let parent=tmp.path().join("projects");std::fs::create_dir(&parent).unwrap();
 let mut settings=Settings::defaults_configured();settings.workspace=Some(tmp.path().into());settings.cron_tick_sec=3600;
 let rt=Runtime::new_with_provider(Arc::new(CliHost::with_data_dir(data.clone())),settings,Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text(r#"{"name":"Go语言学习"}"#)]))).unwrap();
 let sid=rt.create_session(None).unwrap();
 let draft=draft_requested_reply(&data,sid.as_str(),"answer","保存到 Wiki","Go 核心知识：goroutine 和 channel。",2000).unwrap().remove(0);
 let name=rt.wiki_suggest_project_name(&draft.draft_id).await.unwrap();assert_eq!(name,"Go语言学习");
 assert!(!parent.join(&name).exists(),"name generation must not create folders");
 assert_eq!(rt.wiki_suggest_project_name(&draft.draft_id).await.unwrap(),name,"reopening uses persisted model name");
 let result=rt.wiki_confirm_draft(&draft.draft_id,WikiDraftTarget::NewProject{parent:parent.clone(),name:name.clone()},true).unwrap();
 assert_eq!(result.status,"accepted");assert!(parent.join(name).is_dir());
 let pages=rt.wiki_show_for(Some(std::path::Path::new(&result.workspace)),None).unwrap();
 assert!(serde_json::to_string(&pages).unwrap().contains("goroutine"),"saved knowledge must be visible in actual Wiki pages");
}
