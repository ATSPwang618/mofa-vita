"""Offline GPU replay report contracts; fixtures do not execute a GPU."""
import copy
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import vita_gpu_replay as replay


def work(calls, specialized=0, fallback=0):
    return dict(schema=3, scope="successful-native-draws-cumulative",
        coverage_unit="estimated_covered_pixels", totals=[calls,calls,calls*100],
        ops=[[1,calls,calls,calls*100,specialized,fallback]] if calls else [],
        roles_pixel_mask_composite_blur=[[0,0]]*4, selection=[0]*8,
        overflow=[0,0])


def fixture():
    rows = [dict(kind="header",schema=3,build_tag="fixed-test",arm_mhz=444,bus_mhz=222,
        gpu_mhz=222,asset_manifest_sha256="a"*64),
        dict(type="gpu_replay",schema=1,origin="current_production_stage",scene=38,pc=1221,
        update_frame=600,frame_ms_bits=0,time_bits=0,draws=3,sources=2,physical=[960,540],
        plain_input_candidates=[1,0,0],recipe_signature_in_process="0"*16,
        baseline_mask=16,toggle="ps-plain-material",order="ABBA",warm_per_block=30,
        measure_per_block=120,exact_equal=True,
        readback="native_linear_rgba8_after_gpu_and_transfer_finish",
        timing_scope="record CPU; submit+GPU completion; readback separately",
        scope="frozen current Stage; final measured output of each block compared")]
    for i,candidate in enumerate((False,True,True,False)):
        samples = [940 if candidate else 1000]*120
        count = (i+1)*450
        before = work(i*450, i*100, i*100)
        after = work(count,i*100+(150 if candidate else 0),i*100+(0 if candidate else 150))
        rows.append(dict(type="gpu_replay_block",index=i,candidate=candidate,
            mask=16+((1<<30) if candidate else 0),record=dict(n=120),
            submit_complete=dict(n=120),submit_complete_us=samples,capture_us=200,
            fnv1a64="0123456789abcdef",reference="A0",bytes=960*540*4,differing_bytes=0,
            differing_pixels=0,maximum_channel_delta=0,absolute_channel_delta=0,first_byte=None,
            work_counter_scope="warm+measure+capture",executed_work_before=before,executed_work_after=after))
    return rows


def encoded(rows):
    return ("\n".join(json.dumps(row,separators=(",",":")) for row in rows)+"\n").encode()


def full_fixture():
    rows = fixture()
    baseline = 16 | (1 << 30)
    rows[1].update(toggle="ps-full-material", toggle_mask=1 << 37,
        baseline_mask=baseline, full_input_candidates=[1,1,1],
        full_input_candidate_scope="Stage,Pixel,Mask,Composite")
    counts = [0,0,0]
    for block in rows[2:]:
        block["mask"] = baseline | ((1 << 37) if block["candidate"] else 0)
        block["executed_work_before"].update(program_counts=[[54+i,n] for i,n in enumerate(counts) if n], program_omitted=0)
        if block["candidate"]: counts = [n+150 for n in counts]
        block["executed_work_after"].update(program_counts=[[54+i,n] for i,n in enumerate(counts) if n], program_omitted=0)
    return rows


class ReplayAnalysis(unittest.TestCase):
    def test_op7_replays_require_actual_matching_program_execution(self):
        for name,bit,program in [("ps-plain-op7",39,48),("ps-chain-op7",40,57),("ps-material-variants",41,88)]:
            rows=full_fixture();baseline=(1<<4)|(1<<38)|(1<<39) if bit==40 else (1<<4)|(1<<38)
            candidates=[0]*24;candidates[6]=2
            rows[1].update(toggle=name,toggle_mask=1<<bit,baseline_mask=baseline,all_input_candidates=candidates)
            count=0
            for block in rows[2:]:
                block["mask"]=baseline|((1<<bit) if block["candidate"] else 0)
                block["executed_work_before"]["program_counts"]=[[program,count]] if count else []
                if block["candidate"]:count+=150
                block["executed_work_after"]["program_counts"]=[[program,count]] if count else []
            self.assertEqual(replay.analyze(encoded(rows),"hardware")["status"],"pass",name)
            missing=copy.deepcopy(rows)
            for block in missing[2:]:
                block["executed_work_before"]["program_counts"]=[]
                block["executed_work_after"]["program_counts"]=[]
            result=replay.analyze(encoded(missing),"hardware")
            self.assertEqual(result["status"],"inconclusive")
            self.assertIn("no_full_material_program_executed",result["runs"][0]["failure_reasons"])
            unknown=copy.deepcopy(rows);unknown[1]["all_input_candidates"][6]=0
            unknown[1]["all_input_candidates"][0]=2
            self.assertEqual(replay.analyze(encoded(unknown),"hardware")["status"],"pass" if bit==41 else "inconclusive")
    def analyze(self, rows, environment="hardware"):
        return replay.analyze(encoded(rows),environment)

    def test_all_operations_replay_requires_high_bank_program_execution(self):
        rows=full_fixture()
        rows[1].update(toggle="ps-all-operations",toggle_mask=1<<38,all_input_candidates=[1]*24)
        for block in rows[2:]:
            block["mask"]=rows[1]["baseline_mask"] | ((1<<38) if block["candidate"] else 0)
            for key in ("executed_work_before","executed_work_after"):
                block[key]["program_counts"]=[[64+(p-54),n] for p,n in block[key]["program_counts"]]
        result=self.analyze(rows)
        self.assertEqual(result["status"],"pass")
        self.assertEqual(result["runs"][0]["toggle"],"ps-all-operations")
        rows[3]["executed_work_after"]["program_counts"]=rows[3]["executed_work_before"]["program_counts"]
        self.assertEqual(self.analyze(rows)["status"],"inconclusive")

    def test_exact_abba_meets_performance_gate_without_promoting(self):
        result=self.analyze(fixture())
        self.assertEqual(result["status"],"pass")
        run=result["runs"][0]
        self.assertEqual(run["performance"]["baseline"]["n"],240)
        self.assertEqual(run["performance"]["candidate"]["p99_us"],940)
        self.assertAlmostEqual(run["performance"]["median_gain_percent"],6)
        self.assertEqual(run["origin"]["scene"],38)
        self.assertEqual(run["work"][0]["calls"],450)
        self.assertFalse(result["default_promotion_applied"])
        self.assertFalse(result["global_visual_correctness_verified"])

    def test_full_material_uses_bit37_keeps_plain_and_requires_executed_programs(self):
        result = self.analyze(full_fixture())
        self.assertEqual(result["status"],"pass")
        run = result["runs"][0]
        self.assertEqual(run["toggle"],"ps-full-material")
        self.assertEqual(run["full_input_candidates"],[1,1,1])
        self.assertEqual(run["blocks"][1]["work"]["full_programs"],{"54":150,"55":150,"56":150})
        self.assertEqual(run["blocks"][0]["work"]["full_programs"],{"54":0,"55":0,"56":0})

    def test_full_only_profile_and_one_exercised_operation_are_valid(self):
        rows = full_fixture()
        rows[1]["baseline_mask"] &= ~(1 << 30)
        rows[1]["full_input_candidates"] = [1,0,0]
        for block in rows[2:]:
            block["mask"] &= ~(1 << 30)
            for snapshot in ("executed_work_before","executed_work_after"):
                block[snapshot]["program_counts"] = [row for row in block[snapshot]["program_counts"] if row[0] == 54]
        self.assertEqual(self.analyze(rows)["status"],"pass")

    def test_full_missing_counter_coverage_or_unexecuted_program_is_inconclusive(self):
        cases = []
        rows = full_fixture(); rows[1]["full_input_candidates"] = [0,0,0]; cases.append(rows)
        rows = full_fixture(); rows[1].pop("full_input_candidates"); cases.append(rows)
        rows = full_fixture(); rows[2]["executed_work_before"].pop("program_counts"); cases.append(rows)
        rows = full_fixture(); rows[3]["executed_work_after"]["program_omitted"] = 1; cases.append(rows)
        rows = full_fixture()
        for block in rows[2:]:
            for snapshot in ("executed_work_before","executed_work_after"):
                block[snapshot]["program_counts"] = []
        cases.append(rows)
        # A compiler/capability failure in only the second B block also withholds acceptance.
        rows = full_fixture()
        rows[4]["executed_work_after"]["program_counts"] = copy.deepcopy(rows[4]["executed_work_before"]["program_counts"])
        cases.append(rows)
        for case, rows in enumerate(cases):
            with self.subTest(case=case):
                result = self.analyze(rows)
                self.assertEqual(result["status"],"inconclusive")
                self.assertEqual(result["runs"][0]["performance"]["status"],"inconclusive")

    def test_full_toggle_and_cumulative_counter_integrity_fail_closed(self):
        cases = []
        rows = full_fixture(); rows[3]["mask"] ^= 1 << 30; cases.append(rows)
        rows = full_fixture(); rows[1]["baseline_mask"] |= 1 << 37; cases.append(rows)
        rows = full_fixture(); rows[1]["toggle_mask"] = 1 << 30; cases.append(rows)
        rows = full_fixture(); rows[1]["full_input_candidates"] = [True,0,0]; cases.append(rows)
        rows = full_fixture(); rows[3]["executed_work_after"]["program_counts"] = [[54,-1]]; cases.append(rows)
        rows = full_fixture(); rows[3]["executed_work_after"]["program_counts"] = [[54,150],[54,150]]; cases.append(rows)
        rows = full_fixture(); rows[4]["executed_work_after"]["program_counts"] = [[54,1],[55,1],[56,1]]; cases.append(rows)
        rows = full_fixture(); rows[2]["executed_work_after"]["program_counts"] = [[54,1]]; cases.append(rows)
        for case, rows in enumerate(cases):
            with self.subTest(case=case): self.assertEqual(self.analyze(rows)["status"],"inconclusive")

    def test_one_byte_or_alpha_difference_fails(self):
        rows=fixture(); rows[3].update(differing_bytes=1,differing_pixels=1,
            maximum_channel_delta=1,absolute_channel_delta=1,first_byte=3,fnv1a64="f"*16)
        rows[1]["exact_equal"]=False
        result=self.analyze(rows)
        self.assertEqual(result["status"],"fail")
        self.assertIn("rgba8_mismatch",result["runs"][0]["failure_reasons"])

    def test_insufficient_gain_and_tail_regressions_fail_independently(self):
        for samples,reason in (([960]*120,"median_gain_below_5_percent"),
            ([940]*112+[1060]*8,"p95_regression_above_5_percent"),
            ([940]*118+[1110]*2,"p99_regression_above_10_percent")):
            with self.subTest(reason=reason):
                rows=fixture()
                for index in (3,4): rows[index]["submit_complete_us"]=samples
                result=self.analyze(rows)
                self.assertEqual(result["status"],"fail")
                self.assertIn(reason,result["runs"][0]["failure_reasons"])

    def test_missing_block_samples_header_or_exercise_cannot_pass(self):
        cases=[]
        rows=fixture(); rows.pop(); cases.append(rows)
        rows=fixture(); rows[2].pop("submit_complete_us"); cases.append(rows)
        rows=fixture()[1:]; cases.append(rows)
        rows=fixture(); rows[1]["plain_input_candidates"]=[0,0,0]; cases.append(rows)
        rows=fixture()
        for block in rows[2:]: block["executed_work_after"]=copy.deepcopy(block["executed_work_before"])
        cases.append(rows)
        for rows in cases:
            with self.subTest(rows=rows): self.assertEqual(self.analyze(rows)["status"],"inconclusive")

    def test_emulator_and_unspecified_environment_are_not_hardware_acceptance(self):
        for environment in ("emulator","unspecified"):
            result=self.analyze(fixture(),environment)
            self.assertEqual(result["status"],"inconclusive")
            self.assertEqual(result["runs"][0]["performance"]["status"],"pass")

    def test_chunked_prefixed_console_reassembles_and_missing_parts_fail_closed(self):
        rows=fixture(); lines=[]
        for seq,row in enumerate(rows):
            text=json.dumps(row,separators=(",",":"))+"\n"
            parts=[text[i:i+100] for i in range(0,len(text),100)]
            lines.extend("console: "+json.dumps(dict(kind="audit_chunk",seq=seq,part=i,
                total=len(parts),payload=part)) for i,part in enumerate(parts))
        result=replay.analyze(("\n".join(lines)+"\n").encode(),"hardware")
        self.assertEqual(result["status"],"pass")
        result=replay.analyze(("\n".join(lines[:-1])+"\n").encode(),"hardware")
        self.assertEqual(result["status"],"inconclusive")
        self.assertTrue(result["transport_errors"])

    def test_invalid_samples_masks_and_duplicate_blocks_are_rejected(self):
        rows=fixture(); rows[3]["mask"]|=1<<32
        self.assertEqual(self.analyze(rows)["status"],"inconclusive")
        rows=fixture(); rows[2]["submit_complete_us"][0]=True
        self.assertEqual(self.analyze(rows)["status"],"inconclusive")
        rows=fixture(); rows.append(copy.deepcopy(rows[2]))
        self.assertEqual(self.analyze(rows)["status"],"inconclusive")
        rows=fixture(); rows[2]["executed_work_after"]["totals"][0]=-1
        self.assertEqual(self.analyze(rows)["status"],"inconclusive")

    def test_different_replays_remain_separate_and_errors_survive(self):
        rows=fixture()+fixture()
        rows[-5]["pc"]=1300
        rows.append(dict(kind="gpu_replay_error",error="movie active"))
        result=self.analyze(rows)
        self.assertEqual(len(result["runs"]),2)
        self.assertEqual([r["origin"]["pc"] for r in result["runs"]],[1221,1300])
        self.assertEqual(result["status"],"inconclusive")
        self.assertEqual(result["replay_errors"],["movie active"])

    def test_cli_writes_small_report_and_returns_nonzero_for_failed_gate(self):
        with tempfile.TemporaryDirectory() as name:
            path=Path(name)/"capture.log"; path.write_bytes(encoded(fixture()))
            result=subprocess.run([sys.executable,str(Path(replay.__file__)),str(path),
                "--environment","hardware"],capture_output=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr.decode())
            self.assertLess(len(result.stdout),128*1024)
            self.assertEqual(json.loads(result.stdout)["status"],"pass")
            rows=fixture(); rows[3]["submit_complete_us"]=[1100]*120
            path.write_bytes(encoded(rows))
            result=subprocess.run([sys.executable,str(Path(replay.__file__)),str(path),
                "--environment","hardware"],capture_output=True,timeout=10)
            self.assertEqual(result.returncode,1,result.stderr.decode())


if __name__=="__main__": unittest.main()
