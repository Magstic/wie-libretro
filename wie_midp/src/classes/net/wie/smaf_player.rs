use alloc::vec;

use java_class_proto::{JavaFieldProto, JavaMethodProto};
use java_runtime::classes::java::io::InputStream;
use jvm::{ClassInstanceRef, Jvm, Result, runtime::JavaIoInputStream};

use wie_jvm_support::{WieJavaClassProto, WieJvmContext};

// class net.wie.SmafPlayer
pub struct SmafPlayer;

impl SmafPlayer {
    pub fn as_proto() -> WieJavaClassProto {
        WieJavaClassProto {
            name: "net/wie/SmafPlayer",
            parent_class: Some("java/lang/Object"),
            interfaces: vec!["javax/microedition/media/Player"],
            methods: vec![
                JavaMethodProto::new("<init>", "(Ljava/io/InputStream;)V", Self::init, Default::default()),
                JavaMethodProto::new("start", "()V", Self::start, Default::default()),
                JavaMethodProto::new("setLoopCount", "(I)V", Self::set_loop_count, Default::default()),
                JavaMethodProto::new("stop", "()V", Self::stop, Default::default()),
                JavaMethodProto::new("close", "()V", Self::close, Default::default()),
            ],
            fields: vec![
                JavaFieldProto::new("audioHandle", "I", Default::default()),
                JavaFieldProto::new("loopCount", "I", Default::default()),
            ],
            access_flags: Default::default(),
        }
    }

    async fn init(jvm: &Jvm, context: &mut WieJvmContext, mut this: ClassInstanceRef<Self>, stream: ClassInstanceRef<InputStream>) -> Result<()> {
        tracing::debug!("net.wie.SmafPlayer::<init>({this:?}, {stream:?})");

        let _: () = jvm.invoke_special(&this, "java/lang/Object", "<init>", "()V", ()).await?;

        let data = JavaIoInputStream::read_until_end(jvm, &stream).await?;
        let audio_handle = context.system().audio().load_smaf(&data).unwrap();

        jvm.put_field(&mut this, "audioHandle", "I", audio_handle as i32).await?;
        jvm.put_field(&mut this, "loopCount", "I", 1).await?;

        Ok(())
    }

    async fn start(jvm: &Jvm, context: &mut WieJvmContext, this: ClassInstanceRef<Self>) -> Result<()> {
        tracing::debug!("net.wie.SmafPlayer::start({this:?})");

        let audio_handle: i32 = jvm.get_field(&this, "audioHandle", "I").await?;
        let loop_count: i32 = jvm.get_field(&this, "loopCount", "I").await?;

        let system = context.system();

        system.audio().play_with_loop_count(system, audio_handle as u32, loop_count).unwrap();

        Ok(())
    }

    async fn set_loop_count(jvm: &Jvm, _context: &mut WieJvmContext, mut this: ClassInstanceRef<Self>, loop_count: i32) -> Result<()> {
        tracing::debug!("net.wie.SmafPlayer::setLoopCount({this:?}, {loop_count})");

        jvm.put_field(&mut this, "loopCount", "I", loop_count).await?;

        Ok(())
    }

    async fn stop(jvm: &Jvm, context: &mut WieJvmContext, this: ClassInstanceRef<Self>) -> Result<()> {
        tracing::debug!("net.wie.SmafPlayer::stop({this:?})");

        let audio_handle: i32 = jvm.get_field(&this, "audioHandle", "I").await?;

        let system = context.system();

        system.audio().stop(audio_handle as u32);

        Ok(())
    }

    async fn close(jvm: &Jvm, context: &mut WieJvmContext, this: ClassInstanceRef<Self>) -> Result<()> {
        tracing::debug!("net.wie.SmafPlayer::close({this:?})");

        let audio_handle: i32 = jvm.get_field(&this, "audioHandle", "I").await?;

        let system = context.system();

        system.audio().close(audio_handle as u32).unwrap();

        Ok(())
    }
}
