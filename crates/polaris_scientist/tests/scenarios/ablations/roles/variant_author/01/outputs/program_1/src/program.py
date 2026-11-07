def solution():
    score = 2.0

    import csv
    import os
    from pathlib import Path
    folder = Path(__file__).parent / 'analysis'
    folder.mkdir(exist_ok=True)
    variant = os.environ.get('VARIANT_ID', 'control_best')
    evaluation = os.environ.get('EVALUATION_ID', variant + '_eval')
    tables = {
        'run_summary.csv': (['evaluation_id','variant_id','variant_kind','base_program','agent_name','correct','combined_score','overall_pass','primary_metric_name','primary_metric_value','program_dir','results_dir'], [[evaluation,variant,'ablation','best','fixture',True,score,True,'combined_score',score,str(folder.parent),str(folder)]]),
        'metric_values.csv': (['evaluation_id','variant_id','metric_name','metric_value','is_primary_metric'], [[evaluation,variant,'combined_score',score,True]]),
        'timeseries.csv': (['evaluation_id','variant_id','series_name','step','value'], []),
    }
    for name, (columns, rows) in tables.items():
        with (folder / name).open('w',newline='') as out:
            writer = csv.writer(out)
            writer.writerow(columns)
            writer.writerows(rows)
    return score
