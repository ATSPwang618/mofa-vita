"""Generate Mahoyo menu and chapter workloads for batch.py."""
import argparse
import json
from pathlib import Path
import re


def timer(steps, wait_title=True):
    guard = ('if(typeof global.tf == "undefined" || !tf.tt_opened) '
             '{ global.perfTick--; break; }' if wait_title else '')
    body = [f'case 15: {guard} break;']
    for tick, label, code in steps:
        body.append(f'case {tick}: Debug.message("PERF: {label}"); {code} break;')
    return ('global.perfTick=0; global.perfTimer=new Timer(function(e) { '
            'global.perfTick++; try { switch(perfTick) {\n' + '\n'.join(body) +
            '\n} } catch(ex) { Debug.message("PERF-FAIL: "+ex.message); '
            'e.target.enabled=false; } }, ""); '
            'perfTimer.interval=1000; perfTimer.enabled=true;')


def chapter_script(chapter):
    if not re.fullmatch(r'[a-zA-Z0-9_][a-zA-Z0-9_.-]*', chapter):
        raise ValueError('invalid chapter name')
    return '''global.replayTicks=0;
global.replayTimer=new Timer(function(e) {
    global.replayTicks++;
    if(typeof global.tf != "undefined" && tf.tt_opened && replayTicks>=15) {
        e.target.enabled=false;
        Debug.message("PERF: story");
        getArchiveList();
        with(Menu_object) {
            .doInvalidateByInvisible("fore");
            .resetClick(); .storeClick(); .clearClick();
        }
        f.archiveMenuStack=[];
        f.archiveMenuStack.assign(__menuStack);
        outMenu(,true);
        kag.historyOfStore.clear();
        kag.historyLayer.clear();
        f.av_storage=["CHAPTER.ks"];
        kag.process("call.ks","*archive");
    }
}, "");
replayTimer.interval=1000;
replayTimer.enabled=true;
'''.replace('CHAPTER', chapter)


def check(condition, label):
    return (f'if(!({condition})) throw new Exception("{label} not reached"); '
            f'Debug.message("CONFIRMED:{label}"); ')


def screenshots(times):
    return [dict(at_ms=s*1000, action='screenshot') for s in times]


def advance(start, end):
    actions = []
    for at in range(start*1000, end*1000, 600):
        actions += [dict(at_ms=at, action='key_down', key=13),
                    dict(at_ms=at+50, action='key_up', key=13)]
    return actions


def catalog_case():
    return dict(name='catalog', seconds=30, actions=[], expect=['PERF: catalog-complete'], script='''
global.catalogTimer=new Timer(function(e) {
 if(typeof global.tf == "undefined" || !tf.tt_opened) return;
 e.target.enabled=false;
 getArchiveList();
 for(var i=0;i<archive_list.count;i++) {
  for(var j=0;j<archive_list[i][1].count;j++)
   Debug.message("PROFILE_SCENE:"+archive_list[i][1][j].join(","));
 }
 var objects=[]; objects.assign(tracer_object.chart.objects);
 for(var i=0;i<objects.count;i+=2) {
  var block=objects[i+1];
  if(block instanceof "NFEBlockData") {
   if(typeof block.script=="Object" && block.script.count)
    Debug.message("PROFILE_BLOCK:"+block.script.join(","));
   else if(typeof block.script=="String" && block.script!="")
    Debug.message("PROFILE_BLOCK:"+block.script);
  }
 }
 Debug.message("PERF: catalog-complete"); System.exit();
}, "");catalogTimer.interval=1000;catalogTimer.enabled=true;
''')


def cases(chapters, seconds):
    title = [
        (18, 'config-system', 'sf.cf_page=1; openConfigMenu();'),
        (22, 'config-message', check('tf.cf_opened && sf.cf_page==1', 'config-system')+'CFopenPage(2);'),
        (26, 'config-font', check('sf.cf_page==2', 'config-message')+'CFopenFontSelect();'),
        (31, 'config-font-close', check('tf.cf_fontopened', 'config-font')+'CFcloseFontSelect(false);'),
        (35, 'config-sound', 'CFopenPage(3);'),
        (39, 'config-shortcut', check('sf.cf_page==3', 'config-sound')+'CFopenPage(4);'),
        (43, 'config-close', check('sf.cf_page==4', 'config-shortcut')+'closeConfigMenu();'),
        (47, 'title-load', 'openLoadMenu();'),
        (51, 'title-load-page', check('tf.ld_opened', 'title-load')+'changePageSaveMenu(1,"Load");'),
        (55, 'title-load-close', 'closeLoadMenu();'),
        (59, 'archive-open', 'openArchiveMenu();'),
        (64, 'archive-close', check('tf.av_opened', 'archive')+'closeArchiveMenu();'),
    ]
    yield dict(name='title-ui', seconds=69, script=timer(title),
               expect=['CONFIRMED:'+s for s in ['config-system', 'config-message', 'config-font', 'config-sound', 'config-shortcut', 'title-load', 'archive']],
               actions=screenshots([21,25,30,38,42,50,63]))
    ingame = [
        (38, 'menu-open', 'openRClickMenu();'),
        (42, 'menu-close', check('tf.do_systemmenu', 'menu')+'closeRClickMenu();'),
        (46, 'history-open', 'kag.showHistory();'),
        (50, 'history-close', check('kag.historyShowing', 'history')+'kag.hideHistory();'),
        (54, 'save-open', 'openSaveMenu();'),
        (58, 'save-write', check('tf.sv_opened', 'save')+'saveBySaveMenu([0,0]);'),
        (62, 'save-page', 'changePageSaveMenu(1);'),
        (66, 'save-close', 'closeSaveMenu();'),
        (70, 'game-config', 'openConfigMenu();'),
        (74, 'game-config-close', check('tf.cf_opened', 'game-config')+'closeConfigMenu();'),
        (78, 'auto', 'kag.onAutoModeMenuItemClick();'),
        (84, 'auto-stop', check('kag.autoMode', 'auto')+'kag.cancelAutoMode();'),
        (87, 'load-open', 'openLoadMenu();'),
        (91, 'load-slot', check('tf.ld_opened', 'load')+'doLoadByLoadMenu(0);'),
        (100, 'load-finish', check('!tf.ld_opened', 'load-finish')),
    ]
    actions = advance(22,35)+screenshots([41,49,57,61,73,99])
    actions += [dict(at_ms=102000, action='mark', label='PERF: held-skip'),
                dict(at_ms=102000, action='key_down', key=17),
                dict(at_ms=110000, action='key_up', key=17)]
    yield dict(name='ingame-ui', seconds=114, script=chapter_script('5b-12')+'\n'+timer(ingame,False),
               expect=['CONFIRMED:'+s for s in ['menu','history','save','game-config','auto','load','load-finish']],
               actions=sorted(actions,key=lambda a:a['at_ms']))
    extra = [
        # These cases use isolated saves. Unlock gallery/audio eligibility so
        # a missing completion save does not silently skip the expensive views.
        (18, 'cg-open', 'sf.chapter=cgChapterCount; sf.trail["sp1"]=1; openCGMenu();'),
        (24, 'cg-scroll', check('tf.cg_opened', 'cg')+'cgLine.value=1;'),
        (28, 'cg-image', 'openCGImage(0);'),
        (33, 'cg-image-close', check('tf.cg_image_opened', 'cg-image')+'closeCGImage();'),
        (37, 'cg-close', 'closeCGMenu();'),
        (41, 'sound-open', 'sf.playedBGM=%[] if sf.playedBGM==void; '
                          'for(var i=1;i<=55;i++) sf.playedBGM[SMno2filename(i)]=1; openSoundMenu();'),
        (49, 'sound-play', check('tf.sm_opened', 'sound')+'SMmove(1); SMplay();'),
        (53, 'sound-next', check('tf.sm_playing', 'sound-play')+'SMmove(1); SMplay();'),
        (57, 'sound-stop', 'SMstop();'),
        (61, 'sound-close', check('!tf.sm_playing', 'sound-stop')+'closeSoundMenu();'),
        (66, 'teatime-open', 'f.chapter=1; openTeatime();'),
        (72, 'teatime-close', check('tf.ttm_opened', 'teatime')+'closeTeatime();'),
        (76, 'confirm-open', 'askYesNo("返回标题？","确认");'),
        (80, 'confirm-cancel', check('tf.do_askyesno','confirm')+'doneAskYesNo(false);'),
    ]
    yield dict(name='extra-ui', seconds=85, script=timer(extra),
               expect=['CONFIRMED:'+s for s in ['cg','cg-image','sound','sound-play','sound-stop','teatime','confirm']],
               actions=screenshots([23,27,32,48,56,71,79]))
    for chapter in chapters:
        yield dict(name='story-'+chapter, seconds=seconds, script=chapter_script(chapter),
                   pages=True, expect=[chapter+'.ks : *page0'], actions=advance(22,seconds-1))


def full_cases(catalog, seconds, blocks=False):
    groups = []
    archive_scripts = set()
    entries = [json.loads(line) for line in catalog.read_text(encoding='utf-8').splitlines()]
    for event in entries:
        if match := re.search(r'PROFILE_SCENE:([a-zA-Z0-9_.,-]+)', event.get('detail', '')):
            archive_scripts.update(match[1].split(','))
    prefix = 'PROFILE_BLOCK:' if blocks else 'PROFILE_SCENE:'
    for event in entries:
        match = re.search(prefix + r'([a-zA-Z0-9_.,-]+)', event.get('detail', ''))
        if match:
            group = match[1].split(',')
            if blocks and (all(s in archive_scripts for s in group) or group == ['teatime.ks']):
                continue
            if group not in groups:
                groups.append(group)
    if not groups:
        raise ValueError('catalog contains no PROFILE_SCENE entries')
    for index, group in enumerate(groups):
        chapter = group[0].removesuffix('.ks')
        script = chapter_script(chapter).replace('replayTicks>=15', 'replayTicks>=1')
        if blocks:
            # Archive setup expects a block object. The game's lookup returns
            # only its title for string-valued scripts outside the archive.
            # Keep the real block's metadata when sampling those branches.
            script = script.replace('getArchiveList();', '''getArchiveList();
        tracer_object.getBlockFromScript=function(storage) {
            var objects=[]; objects.assign(tracer_object.chart.objects);
            for(var i=0;i<objects.count;i+=2) {
                var b=objects[i+1];
                if(!(b instanceof "NFEBlockData")) continue;
                if(typeof b.script=="String" && b.script==storage) return b;
                if(typeof b.script=="Object")
                    for(var j=0;j<b.script.count;j++)
                        if(b.script[j]==storage) return b;
            }
        };''')
        script = script.replace('f.av_storage=["'+chapter+'.ks"];',
            'f.av_storage='+json.dumps(group)+'; f.av_readall=0; f.av_no=-1; '
            'kag.userChSpeed=0; kag.userCh2ndSpeed=0; kag.setUserSpeed(); '
            f'global.perfStoryCount={len(group)}; global.perfStoryStarted=true;')
        script += '''
global.perfStoryStarted=false;
global.fullReplayTimer=new Timer(function(e) {
 if(!perfStoryStarted) return;
 if(f.av_no>=perfStoryCount) {
  e.target.enabled=false;
  Debug.message("PERF: complete");
  System.exit();
  return;
 }
 // Advance only an actual text/click wait. Do not send Return into an
 // animation: the game's keyboard handler may cut that effect short.
 if(kag.clickWaiting) kag.onPrimaryClick();
}, "");fullReplayTimer.interval=100;fullReplayTimer.enabled=true;
'''
        yield dict(name=f'{"block" if blocks else "full"}-{index:02}-{chapter.replace(".", "_")}', seconds=seconds,
                   script=script, pages=True, story_scripts=group,
                   mode=('isolated flowchart block; ' if blocks else 'archive replay; ')+
                        'instant text, normal effects, automatic click waits',
                   expect=['PERF: complete'], actions=[])


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--game', required=True)
    parser.add_argument('--savedata', required=True, help='copy of saves with the desired unlocked scenes')
    parser.add_argument('--gles-dir', required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--chapters', nargs='*', default=['1-0','5b-12','5b-14','5b-15','5a-8'])
    parser.add_argument('--seconds', type=int, default=60)
    parser.add_argument('--catalog', type=Path, help='events.jsonl with PROFILE_SCENE archive lists')
    parser.add_argument('--catalog-only', action='store_true', help='record the runtime archive and flowchart lists')
    parser.add_argument('--extra-blocks', action='store_true', help='sample flowchart blocks outside the archive as isolated replays')
    parser.add_argument('--full-timeout', type=int, default=900, help='maximum seconds per complete archive replay')
    args=parser.parse_args()
    if args.seconds<25:
        parser.error('--seconds must allow startup (at least 25)')
    manifest=dict(game=args.game,savedata=args.savedata,gles_dir=args.gles_dir,
                  startup='启动游戏.xp3>startup.tjs',at9_clock=True,
                  cases=list(cases([] if args.catalog else args.chapters,args.seconds)))
    if args.catalog_only:
        manifest['cases'] = [catalog_case()]
    elif args.catalog:
        if args.extra_blocks:
            manifest['cases'] = []
        manifest['cases'] += list(full_cases(args.catalog,args.full_timeout,args.extra_blocks))
    args.out.parent.mkdir(parents=True,exist_ok=True)
    args.out.write_text(json.dumps(manifest,ensure_ascii=False,indent=2),encoding='utf-8')


if __name__=='__main__':
    main()
