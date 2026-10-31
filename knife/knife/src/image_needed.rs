//! Nonblocking VM image demand. The caller keeps its PC and old stage until
//! Ready; Failed is a real error, never permission to skip the instruction.
use super::*;
use std::time::{Duration,Instant};
use knife_platform_vita::image_worker::{WorkerPoll,resource_state::{self,Phase}};

pub enum ImageNeeded {Ready(Arc<Image>),Pending,Failed(String)}
#[derive(Clone,Copy)]
enum Step {Source,Decode}
pub(super) struct NeededRequest {
    key:String,name:String,strip:Option<(usize,usize)>,archive:Arc<Archive>,index:usize,
    generation:u64,started:Instant,step:Step,failure:Option<String>,retry_source:bool,
}
const DEADLINE:Duration=Duration::from_secs(10);
impl Assets {
    /// Directory lookup only. A missing variant may select its base; a Failed
    /// load of an existing variant must be reported instead of silently falling
    /// back to different artwork.
    pub fn has_image_source(&self,name:&str)->bool {
        Self::primary_image_locator(&self.image_archives(),&name.to_ascii_uppercase()).is_some()
    }
    /// Scene/seek/load changes retire the old logical request. Active readers
    /// keep their immutable owner/reservation until completion; their old CPU
    /// decode results cannot enter this new generation.
    pub fn reset_image_needed_context(&mut self) {
        self.image_needed=None;self.image_context=self.image_context.wrapping_add(1).max(1);
        if let Some(worker)=&self.image_worker {worker.retain_window(self.image_context,&[]);}
        resource_state::demand(Phase::Idle,"",0,usize::MAX,self.image_context);
    }
    fn image_need_failed(&mut self,error:String)->ImageNeeded {
        if let Some(request)=&mut self.image_needed {
            if request.failure.is_none() {
                tracing::error!(name=%request.name,generation=request.generation,"image demand failed: {error}");
                request.failure=Some(error.clone());
                if let Some(worker)=&self.image_worker {worker.release_needed(&request.key);}
            }
            resource_state::demand(Phase::DemandFailed,&request.key,request.archive.cache_identity(),request.index,request.generation);
        }
        ImageNeeded::Failed(error)
    }
    fn finish_needed_pixels(&mut self,r:&Renderer,key:String,name:&str,pixels:Result<Pixels>)->ImageNeeded {
        let mut pixels=match pixels {Ok(pixels)=>pixels,Err(error)=>return self.image_need_failed(format!("{name}: {error:#}"))};
        if let Pixels::GxtNative {first_use,..}=&mut pixels {*first_use=true;}
        let (archive,index)=self.image_needed.as_ref().map_or((0,usize::MAX),|request|(request.archive.cache_identity(),request.index));
        resource_state::demand(Phase::DemandRegister,&key,archive,index,self.image_context);
        match self.finish_image(r,key.clone(),name,Ok(pixels)) {
            Some(image)=>{
                if let Some(worker)=&self.image_worker {worker.release_needed(&key);}
                self.image_needed=None;resource_state::demand(Phase::Idle,"",0,usize::MAX,self.image_context);
                ImageNeeded::Ready(image)
            },
            None=>self.image_need_failed(format!("{name}: image registration/upload failed")),
        }
    }
    pub fn poll_image_needed(&mut self,r:&Renderer,name:&str,bake:Bake)->ImageNeeded {
        let (name,grade,key)=Self::image_key(name,bake);
        if let Some(request)=self.image_needed.as_ref().filter(|request|request.key==key)
            && let Some(error)=request.failure.clone() {return ImageNeeded::Failed(error);}
        if let Some(image)=self.images.get(&key).and_then(Option::as_ref).cloned() {
            self.image_cache.touch(&key);
            // STCH retries may first read an already cached NONE source before
            // polling its copied/graded source. That cache hit must not cancel
            // the distinct pending Bake or restart its original deadline.
            if self.image_needed.as_ref().is_some_and(|request|request.key==key) {
                self.image_needed=None;if let Some(worker)=&self.image_worker {worker.release_needed(&key);}
                resource_state::demand(Phase::Idle,"",0,usize::MAX,self.image_context);
            }
            return ImageNeeded::Ready(image);
        }
        if self.image_needed.as_ref().is_some_and(|request|request.key!=key) {self.reset_image_needed_context();}
        if self.image_needed.is_none() {
            let Some((archive,index))=Self::primary_image_locator(&self.image_archives(),&name)else{
                return ImageNeeded::Failed(format!("image source missing: {name}"));
            };
            self.image_needed=Some(NeededRequest {key:key.clone(),name:name.clone(),strip:self.strip(&name),
                archive,index,generation:self.image_context,started:Instant::now(),step:Step::Source,failure:None,retry_source:true});
        }
        let request=self.image_needed.as_ref().unwrap();
        if let Some(error)=request.failure.clone() {return ImageNeeded::Failed(error);}
        if request.started.elapsed()>=DEADLINE {
            return self.image_need_failed(format!("image demand deadline (10s): {name}; active workers retained"));
        }
        let (archive,index,strip,generation,step)=(request.archive.clone(),request.index,request.strip,request.generation,request.step);
        // A speculative decode may already have completed. Consume exactly its
        // current-generation result; prefetch budget rejection is not a real
        // demand failure and is retried once through the larger demand cap.
        if let Some(worker)=&self.image_worker {
            match worker.poll_needed(generation,&key) {
                WorkerPoll::Ready(Some(decoded)) if decoded.strip==strip=>{
                    if decoded.pixels.is_err() && !decoded.demanded {worker.release_needed(&key);self.image_needed.as_mut().unwrap().step=Step::Source;}
                    else{return self.finish_needed_pixels(r,key,&name,decoded.pixels);}
                },
                WorkerPoll::Ready(_)=>{worker.release_needed(&key);self.image_needed.as_mut().unwrap().step=Step::Source;},
                WorkerPoll::Failed(error) if matches!(step,Step::Decode)=>return self.image_need_failed(format!("{name}: {error}")),
                WorkerPoll::Pending if matches!(step,Step::Decode)=>return ImageNeeded::Pending,
                WorkerPoll::Missing if matches!(step,Step::Decode)=>{self.image_needed.as_mut().unwrap().step=Step::Source;},
                _=>{},
            }
        }else if matches!(step,Step::Decode) {return self.image_need_failed(format!("image CPU worker unavailable: {name}"));}
        resource_state::demand(Phase::DemandSource,&key,archive.cache_identity(),index,generation);
        if self.image_needed.as_ref().unwrap().retry_source {
            if !self.atlas_cache.retry_failed(&archive,index) {return ImageNeeded::Pending;}
            self.image_needed.as_mut().unwrap().retry_source=false;
        }
        let data=match self.atlas_cache.poll(archive.clone(),index,true) {
            atlas_cache::Poll::Ready(bytes)=>bytes,
            atlas_cache::Poll::Pending=>return ImageNeeded::Pending,
            atlas_cache::Poll::Failed(error)=>return self.image_need_failed(format!("{name}: {error}")),
        };
        let native=compact::parse(&data).ok().is_some_and(|image|
            gxt_native_route(image.encoding==compact::Encoding::GxtNativeUnorm,grade.cpu(),strip.is_some(),grade.raw));
        if native {
            // Validated bytes are already in the source pool. Plain GXT needs
            // no CPU codec worker, RGBA allocation or eager GPU upload.
            let pixels=Self::decode_shared_data_pixels(data,archive,index,&name,grade,strip,None);
            return self.finish_needed_pixels(r,key,&name,pixels);
        }
        resource_state::demand(Phase::DemandDecode,&key,archive.cache_identity(),index,generation);
        let Some(worker)=&self.image_worker else{return self.image_need_failed(format!("image CPU worker unavailable: {name}"));};
        if !worker.is_running() {return self.image_need_failed(format!("image CPU worker unavailable: {name}"));}
        let job=ImageJob {initial:Some((data,archive,index)),demanded:true,key:key.clone(),name,grade,strip};
        if worker.request_needed(generation,key,job) {self.image_needed.as_mut().unwrap().step=Step::Decode;}
        ImageNeeded::Pending
    }
}
