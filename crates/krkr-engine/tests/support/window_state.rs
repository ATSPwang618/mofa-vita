use super::*;

#[test]
fn video_geometry_accepts_script_zoom_results() {
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        var w=new Window(), movie=new VideoOverlay(w);
        movie.setBounds(3.9,-4.8,800/1.5,600/1.5);
        if(movie.left!=3 || movie.top!=-4 || movie.width!=533 || movie.height!=400)
            throw 'scaled bounds';
        movie.setPos('12',-9.5);movie.setSize(123.75,'456');
        if(movie.left!=12 || movie.top!=-9 || movie.width!=123 || movie.height!=456)
            throw 'coerced geometry';
        movie.left=-2.5;movie.top='3';movie.width='20';movie.height=10.8;
        if(movie.left!=-2 || movie.top!=3 || movie.width!=20 || movie.height!=10)
            throw 'geometry setters';
        invalidate w;'ok';
    "#,
            1
        ),
        "ok"
    );
}

#[test]
fn kag_layer_can_initialize_position_before_base_constructor() {
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        class EarlyLayer extends Layer {
            function EarlyLayer(w, p) {
                left=0; top=0;
                if(left!=0 || top!=0) throw 'native initial position';
                super.Layer(w,p);
            }
        }
        var w=new Window(), root=new Layer(w,null), child=new EarlyLayer(w,root);
        child.left=17; child.top=-9;
        if(child.left!=17 || child.top!=-9) throw 'constructed position';
        invalidate child;
        var rejected=false;try {child.left=0;} catch(e) {rejected=true;}
        if(!rejected) throw 'invalidated layer accepted write';
        invalidate w;'ok';
    "#,
            1
        ),
        "ok"
    );
}

#[test]
fn layout_constraints_resize_even_when_host_only_accepts_hints() {
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        class LayoutWindow extends Window {
            function LayoutWindow() {
                innerSunken=true;showScrollBars=false;super.Window();
            }
        }
        var w=new LayoutWindow();
        if(!w.innerSunken || w.showScrollBars) throw 'preconstructor state';
        var other=new Window();
        if(other.innerSunken || !other.showScrollBars) throw 'shared state';
        w.setInnerSize(1,1);
        w.setMinSize(408,492);
        if(w.innerWidth!=392 || w.innerHeight!=453) throw 'minimum did not resize';
        w.setMaxSize(408,492);
        w.setInnerSize(900,900);
        if(w.width!=408 || w.height!=492) throw 'requested size escaped constraints';
        w.setMinSize(0,0);w.setMaxSize(316,239);
        if(w.innerWidth!=300 || w.innerHeight!=200) throw 'maximum did not resize';
        w.setMaxSize(0,0);w.setInnerSize(900,650);
        if(w.innerWidth!=900 || w.innerHeight!=650) throw 'constraint removal';
        invalidate w;invalidate other;'ok';
    "#,
            1
        ),
        "ok"
    );
}

#[test]
fn kag_window_setup_and_layer_focus_use_the_shared_host_and_callback_paths() {
    let (mut engine, mut host, _) = setup();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "var unavailable=false;try{System.screenWidth;}catch(e){unavailable=true;}unavailable;",
            1
        ),
        "1"
    );
    host.host.set_display(Some(window::Display {
        width: 1920,
        height: 1080,
        desktop_left: -1920,
        desktop_top: 30,
        desktop_width: 1920,
        desktop_height: 1050,
    }));
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        if(System.screenWidth!=1920 || System.screenHeight!=1080 || System.desktopLeft!=-1920 ||
           System.desktopTop!=30 || System.desktopWidth!=1920 || System.desktopHeight!=1050) throw 'display';
        var denied=false;try{System.screenWidth=1;}catch(e){denied=true;}
        if(!denied) throw 'screen is read-only';
        var w=new Window();
        if(w.borderStyle!=bsSizeable || w.focusedLayer!==null || w.innerSunken || !w.showScrollBars) throw 'window defaults';
        w.borderStyle=bsSingle;w.innerSunken=true;w.showScrollBars=false;
        w.setInnerSize(800,600);w.beginMove();
        if(w.innerWidth!=800 || w.innerHeight!=600 || !w.innerSunken || w.showScrollBars) throw 'KAG geometry';
        w.borderStyle=bsNone;w.borderStyle=bsDialog;w.borderStyle=bsToolWindow;
        w.borderStyle=bsSizeToolWin;w.borderStyle=bsSizeable;
        denied=false;try{w.borderStyle=123;}catch(e){denied=true;}
        if(!denied || w.borderStyle!=bsSizeable) throw 'invalid style committed';
        var root=new Layer(w,null), a=new Layer(w,root), b=new Layer(w,root), log='';
        if(root.window!==w || a.window!==w) throw 'layer owner';
        a.visible=b.visible=true;a.focusable=b.focusable=true;
        w.action=function(e){
            if(e.type=='onBeforeFocus') System.wait(0);
            if(e.type=='onBlur') global.log+='blur;';
            if(e.type=='onFocus') global.log+='focus;';
        };
        w.focusedLayer=a;w.focusedLayer=b;
        if(w.focusedLayer!==b || a.focused || !b.focused || log!='focus;blur;focus;') throw 'focus callbacks';
        b.onBeforeFocus=function(layer,blurred,direction){
            System.wait(0);(global.Layer.onBeforeFocus incontextof this)(global.a,blurred,direction);
        };
        w.focusedLayer=null;w.focusedLayer=b;
        if(w.focusedLayer!==a) throw 'redirect';
        var other=new Window(), foreign=new Layer(other,null);
        denied=false;try{w.focusedLayer=foreign;}catch(e){denied=true;}
        if(!denied || w.focusedLayer!==a || other.focusedLayer!==null) throw 'foreign focus';
        w.focusedLayer=void;
        if(w.focusedLayer!==null || a.focused) throw 'clear focus';
        invalidate w;
        if(root.window!==null || a.window!==null) throw 'closed owner';
        invalidate other;
        'ok';
    "#,
            1
        ),
        "ok"
    );
    assert_eq!(
        host.borders,
        [
            window::BorderStyle::Single,
            window::BorderStyle::None,
            window::BorderStyle::Dialog,
            window::BorderStyle::ToolWindow,
            window::BorderStyle::SizeToolWin,
            window::BorderStyle::Sizeable
        ]
    );
    assert_eq!(host.moves, 1);
    host.host.set_display(Some(window::Display {
        width: 2560,
        height: 1440,
        desktop_left: 0,
        desktop_top: 0,
        desktop_width: 2560,
        desktop_height: 1400,
    }));
    assert_eq!(
        run(&mut engine, &mut host, "System.screenWidth;", 1),
        "2560"
    );
}
